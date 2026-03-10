//! Scrcpy Server 生命周期管理
//!
//! 负责将 scrcpy-server JAR 推送到设备、端口转发、启动服务、建立 TCP 连接。
//! 使用 peek 就绪检测 + 指数退避重试。

use crate::connection::adb::{adb_command, run_adb_timed};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Mutex;
use tokio::net::TcpStream;

/// 全局端口池：回收已释放的端口，避免端口耗尽
static PORT_POOL: Mutex<VecDeque<u16>> = Mutex::new(VecDeque::new());
/// 回退分配器：仅在池为空时使用
static NEXT_PORT: AtomicU16 = AtomicU16::new(27183);

/// 端口分配范围上限（最多 1000 个并发投屏）
const PORT_RANGE_MAX: u16 = 28182;

fn allocate_port() -> Result<u16, String> {
    let mut pool = PORT_POOL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(port) = pool.pop_front() {
        return Ok(port);
    }
    // fetch_update 确保不会超出范围后仍递增
    NEXT_PORT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |p| {
            if p < PORT_RANGE_MAX { Some(p + 1) } else { None }
        })
        .map_err(|_| "端口池耗尽 (最大 1000 个并发投屏)".to_string())
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
/// start() 全局超时秒数（SG-1）
const START_TIMEOUT_SECS: u64 = 30;

/// Scrcpy 服务端实例（管理一台设备的投屏连接）
pub struct ScrcpyServer {
    pub serial: String,
    pub port: u16,
    pub screen_width: u32,
    pub screen_height: u32,
    pub video_stream: Option<TcpStream>,
    pub control_stream: Option<TcpStream>,
    child: Option<tokio::process::Child>,
    /// C-3 修复：持有 stderr drain 任务句柄
    stderr_task: Option<tokio::task::JoinHandle<()>>,
}

impl ScrcpyServer {
    /// 启动 scrcpy-server 并建立连接
    /// SG-1: 全局 30s 超时保护
    pub async fn start(serial: &str, jar_path: &str) -> Result<Self, String> {
        tokio::time::timeout(
            std::time::Duration::from_secs(START_TIMEOUT_SECS),
            Self::start_inner(serial, jar_path),
        )
        .await
        .map_err(|_| format!("投屏连接超时 ({}s)", START_TIMEOUT_SECS))?
    }

    async fn start_inner(serial: &str, jar_path: &str) -> Result<Self, String> {
        let port = allocate_port()?;

        // 1. 检测 JAR 是否已存在且版本一致，仅首次推送
        let s = serial.to_string();
        let j = jar_path.to_string();
        tokio::task::spawn_blocking(move || Self::push_jar_if_needed(&s, &j))
            .await
            .map_err(|e| format!("push_jar 任务失败: {}", e))??;

        // 2. 端口转发
        let s = serial.to_string();
        tokio::task::spawn_blocking(move || Self::forward_port(&s, port))
            .await
            .map_err(|e| format!("forward_port 任务失败: {}", e))??;

        // 3. 启动 app_process
        let (child, stderr) = Self::spawn_server(serial).await?;

        // C-3 修复：stderr drain 任务句柄持有，stop() 时 abort
        let stderr_handle = tokio::spawn(Self::drain_stderr(stderr));

        // 给 scrcpy-server 短暂初始化时间（bind abstract socket）
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        // 4. 连接 video socket（peek 验证 server 发送数据后才算就绪）
        let mut video_stream = Self::connect_tcp(port, "video", true).await?;

        // 5. 连接 control socket（连上即可，server 不主动发数据）
        let control_stream = Self::connect_tcp(port, "control", false).await?;

        // 6. 消费 video header
        let _device_name = super::video::read_device_name(&mut video_stream).await?;
        let header = super::video::read_video_header(&mut video_stream).await?;

        // 7. 获取实际屏幕尺寸 (P-4: spawn_blocking 避免阻塞 async runtime)
        let (screen_w, screen_h) = {
            let s = serial.to_string();
            let vw = header.width;
            let vh = header.height;
            tokio::task::spawn_blocking(move || Self::get_screen_size(&s, vw, vh))
                .await
                .unwrap_or((header.width, header.height))
        };

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
            stderr_task: Some(stderr_handle),
        })
    }

    /// 停止服务端，清理资源
    pub async fn stop(&mut self) {
        // C-3: abort stderr drain 任务
        if let Some(h) = self.stderr_task.take() {
            h.abort();
        }

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

        // 不再删除设备上的 JAR，下次启动时复用

        // C-1: 归还端口到池中
        release_port(self.port);

        eprintln!("[scrcpy] 清理完成: {}", self.serial);
    }

    // ── 内部方法 ──

    /// SG-5: 检测设备上是否已有 scrcpy-server JAR 且大小一致，不一致时重新推送
    fn push_jar_if_needed(serial: &str, jar_path: &str) -> Result<(), String> {
        // 获取本地 JAR 文件大小
        let local_size = std::fs::metadata(jar_path).map(|m| m.len()).unwrap_or(0);

        let check = run_adb_timed(
            adb_command().args([
                "-s",
                serial,
                "shell",
                &format!(
                    "[ -f '{}' ] && stat -c %s '{}' || echo MISSING",
                    DEVICE_JAR_PATH, DEVICE_JAR_PATH
                ),
            ]),
            3,
        );

        if let Ok(output) = check {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // 如果返回的是数字（文件大小），比较是否与本地一致
            if let Ok(remote_size) = stdout.parse::<u64>() {
                if remote_size == local_size && local_size > 0 {
                    eprintln!("[scrcpy] JAR 已存在且大小一致 ({}B)，跳过推送", local_size);
                    return Ok(());
                }
                eprintln!(
                    "[scrcpy] JAR 大小不一致 (本地={}B, 设备={}B)，重新推送",
                    local_size, remote_size
                );
            }
            // 否则 stdout 是 "MISSING" 或其他，需要推送
        }

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
        let mut cmd = tokio::process::Command::new(&adb_path);
        cmd.args([
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
            .stderr(std::process::Stdio::piped());

        // Windows: 隐藏控制台窗口
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }

        let mut child = cmd.spawn()
            .map_err(|e| format!("启动 scrcpy-server 失败: {}", e))?;

        let stderr = child.stderr.take().ok_or("无法获取 server stderr")?;
        Ok((child, stderr))
    }

    /// 后台异步读取 stderr 日志（不阻塞连接流程）
    async fn drain_stderr(stderr: tokio::process::ChildStderr) {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            eprintln!("[scrcpy-server] {}", line);
        }
    }

    /// 连接 TCP socket，带 backoff 重试
    /// verify_data: video socket 需要 peek 验证 server 发了数据；control socket 连上即可
    async fn connect_tcp(port: u16, label: &str, verify_data: bool) -> Result<TcpStream, String> {
        let addr = format!("127.0.0.1:{}", port);
        let max_retries = 15;
        let mut last_err = String::new();
        let mut delay_ms = 100u64;

        for i in 0..max_retries {
            match TcpStream::connect(&addr).await {
                Ok(stream) => {
                    if verify_data {
                        // video: peek 验证 server 已发送数据
                        let mut probe = [0u8; 1];
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(500),
                            stream.peek(&mut probe),
                        )
                        .await
                        {
                            Ok(Ok(n)) if n > 0 => {
                                eprintln!("[scrcpy] {} socket 已连接 (尝试 {})", label, i + 1);
                                return Ok(stream);
                            },
                            _ => {
                                drop(stream);
                                last_err = "server 未就绪".to_string();
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                                    .await;
                                delay_ms = (delay_ms * 2).min(2000);
                                continue;
                            },
                        }
                    } else {
                        // control: TCP 连上即可
                        eprintln!("[scrcpy] {} socket 已连接 (尝试 {})", label, i + 1);
                        return Ok(stream);
                    }
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

    /// P-4: 获取屏幕实际尺寸（同步方法，必须在 spawn_blocking 中调用）
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
        // C-3: abort stderr task on drop
        if let Some(h) = self.stderr_task.take() {
            h.abort();
        }
        if let Some(ref mut child) = self.child {
            let _ = child.start_kill();
        }
    }
}
