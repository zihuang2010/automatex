//! Scrcpy Server 生命周期管理
//!
//! 负责将 scrcpy-server JAR 推送到设备、端口转发、启动服务、建立 TCP 连接。
//! 使用 stderr 就绪检测代替固定 sleep。

use crate::connection::adb::{adb_command, run_adb_timed};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;
use tokio::net::TcpStream;

// ─── C-1 修复：端口池 ─────────────────────────────────────────

/// 全局端口池：回收已释放的端口，避免端口耗尽
static PORT_POOL: Mutex<VecDeque<u16>> = Mutex::new(VecDeque::new());
/// 回退分配器：仅在池为空时使用
static NEXT_PORT: AtomicU16 = AtomicU16::new(27183);

fn allocate_port() -> u16 {
    let mut pool = PORT_POOL.lock().unwrap_or_else(|e| e.into_inner());
    pool.pop_front().unwrap_or_else(|| NEXT_PORT.fetch_add(1, Ordering::Relaxed))
}

fn release_port(port: u16) {
    if let Ok(mut pool) = PORT_POOL.lock() {
        pool.push_back(port);
    }
}

/// scrcpy-server 的版本号（与 JAR 文件版本匹配）
const SCRCPY_VERSION: &str = "3.3.4";
/// 设备上 JAR 的路径
const DEVICE_JAR_PATH: &str = "/data/local/tmp/scrcpy-server";

/// Scrcpy 服务端实例（管理一台设备的投屏连接）
pub struct ScrcpyServer {
    pub serial: String,
    pub port: u16,
    pub screen_width: u32,
    pub screen_height: u32,
    pub video_stream: Option<TcpStream>,
    pub control_stream: Option<TcpStream>,
    child: Option<tokio::process::Child>,
}

impl ScrcpyServer {
    /// 启动 scrcpy-server 并建立连接
    pub async fn start(serial: &str, jar_path: &str) -> Result<Self, String> {
        let port = allocate_port();

        // 1. Push JAR 到设备（Q-1 修复：spawn_blocking 避免阻塞 async runtime）
        let s = serial.to_string();
        let j = jar_path.to_string();
        tokio::task::spawn_blocking(move || Self::push_jar(&s, &j))
            .await
            .map_err(|e| format!("push_jar 任务失败: {}", e))??;

        // 2. 端口转发（Q-1 修复：spawn_blocking）
        let s = serial.to_string();
        tokio::task::spawn_blocking(move || Self::forward_port(&s, port))
            .await
            .map_err(|e| format!("forward_port 任务失败: {}", e))??;

        // 3. 启动 app_process
        let (child, stderr) = Self::spawn_server(serial).await?;

        // 优雅等待：监听 stderr 直到 scrcpy server 输出就绪信号
        Self::wait_server_ready(stderr).await;

        // 4. 连接 video socket（第一个连接）
        let mut video_stream = Self::connect_tcp(port, "video").await?;

        // 5. 连接 control socket（第二个连接）
        let control_stream = Self::connect_tcp(port, "control").await?;

        // 6. 消费 video header
        let _device_name = super::video::read_device_name(&mut video_stream).await?;
        let header = super::video::read_video_header(&mut video_stream).await?;

        // 7. 获取实际屏幕尺寸
        let (screen_w, screen_h) = Self::get_screen_size(serial, header.width, header.height);

        eprintln!(
            "[scrcpy] server 启动成功: serial={}, port={}, screen={}x{}, video={}x{}",
            serial, port, screen_w, screen_h, header.width, header.height
        );

        Ok(Self {
            serial: serial.to_string(),
            port,
            screen_width: screen_w,
            screen_height: screen_h,
            video_stream: Some(video_stream),
            control_stream: Some(control_stream),
            child: Some(child),
        })
    }

    /// 停止服务端，清理资源
    pub async fn stop(&mut self) {
        // Kill 进程
        if let Some(ref mut child) = self.child {
            let _ = child.kill().await;
            eprintln!("[scrcpy] 已终止 server 进程: {}", self.serial);
        }
        self.child = None;

        // 关闭 streams（drop 即可）
        self.video_stream = None;
        self.control_stream = None;

        // 移除端口转发
        let port_str = format!("tcp:{}", self.port);
        let serial = self.serial.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = run_adb_timed(
                adb_command().args(["-s", &serial, "forward", "--remove", &port_str]),
                5,
            );
        })
        .await;

        // 删除设备上的 JAR
        let serial = self.serial.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = run_adb_timed(
                adb_command().args(["-s", &serial, "shell", "rm", "-f", DEVICE_JAR_PATH]),
                5,
            );
        })
        .await;

        // C-1 修复：归还端口到池中
        release_port(self.port);

        eprintln!("[scrcpy] 清理完成: {}", self.serial);
    }

    // ── 内部方法 ──

    fn push_jar(serial: &str, jar_path: &str) -> Result<(), String> {
        eprintln!("[scrcpy] 推送 JAR: {} -> {}", jar_path, DEVICE_JAR_PATH);
        run_adb_timed(adb_command().args(["-s", serial, "push", jar_path, DEVICE_JAR_PATH]), 30)?;
        Ok(())
    }

    fn forward_port(serial: &str, port: u16) -> Result<(), String> {
        let local = format!("tcp:{}", port);
        let remote = "localabstract:scrcpy";
        eprintln!("[scrcpy] 端口转发: {} -> {}", local, remote);
        run_adb_timed(adb_command().args(["-s", serial, "forward", &local, remote]), 10)?;
        Ok(())
    }

    async fn spawn_server(
        serial: &str,
    ) -> Result<(tokio::process::Child, tokio::process::ChildStderr), String> {
        let adb_path = crate::connection::adb::adb_path().to_string();
        let mut child = tokio::process::Command::new(&adb_path)
            .args([
                "-s",
                serial,
                "shell",
                &format!(
                    "CLASSPATH={} app_process / com.genymobile.scrcpy.Server {} \
                     tunnel_forward=true video=true audio=false control=true \
                     video_codec=h264 max_size=0 max_fps=30",
                    DEVICE_JAR_PATH, SCRCPY_VERSION
                ),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 scrcpy-server 失败: {}", e))?;

        let stderr = child.stderr.take().ok_or("无法获取 server stderr")?;
        Ok((child, stderr))
    }

    /// 优雅等待 scrcpy server 就绪：读取 stderr 检测启动信号
    /// scrcpy-server 启动时会在 stderr 输出 "[server]" 或 "INFO:" 行
    /// 超时 5 秒后回退到直接连接（兼容不同版本）
    async fn wait_server_ready(stderr: tokio::process::ChildStderr) {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let timeout = std::time::Duration::from_secs(5);

        let result = tokio::time::timeout(timeout, async {
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[scrcpy-server] {}", line);
                // scrcpy v3.x 输出 "[server] INFO: ..." 表示就绪
                if line.contains("[server]") || line.contains("INFO:") {
                    return;
                }
            }
        })
        .await;

        if result.is_err() {
            eprintln!("[scrcpy] 等待 server 就绪超时 ({}s)，尝试直接连接", timeout.as_secs());
        }
    }

    async fn connect_tcp(port: u16, label: &str) -> Result<TcpStream, String> {
        let addr = format!("127.0.0.1:{}", port);
        let max_retries = 12;
        let mut last_err = String::new();
        let mut delay_ms = 200u64;

        for i in 0..max_retries {
            match TcpStream::connect(&addr).await {
                Ok(stream) => {
                    eprintln!("[scrcpy] {} socket 已连接 (尝试 {})", label, i + 1);
                    return Ok(stream);
                },
                Err(e) => {
                    last_err = format!("{}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    delay_ms = (delay_ms * 2).min(2000);
                },
            }
        }

        Err(format!("连接 {} socket 失败 ({}次尝试): {}", label, max_retries, last_err))
    }

    /// 获取屏幕实际尺寸，如果视频头给出了尺寸就用它，否则从 adb 查询
    fn get_screen_size(serial: &str, video_w: u32, video_h: u32) -> (u32, u32) {
        if video_w > 0 && video_h > 0 {
            return (video_w, video_h);
        }

        // 后备：通过 adb shell wm size 获取
        match run_adb_timed(adb_command().args(["-s", serial, "shell", "wm", "size"]), 5) {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(size_part) = stdout.split(':').nth(1) {
                    let parts: Vec<&str> = size_part.trim().split('x').collect();
                    if parts.len() == 2 {
                        let w = parts[0].parse::<u32>().unwrap_or(video_w);
                        let h = parts[1].parse::<u32>().unwrap_or(video_h);
                        return (w, h);
                    }
                }
                (video_w, video_h)
            },
            Err(_) => (video_w, video_h),
        }
    }
}

impl Drop for ScrcpyServer {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            let _ = child.start_kill();
        }
    }
}
