//! 本机网络接口诊断
//!
//! 用于 USB→WiFi 切换前的 preflight：
//! 在让手机执行 `adb tcpip 5555`（会立刻断开 USB）之前，先确认本机能到达手机所在 WiFi 网段。
//!
//! 不做实际网络 IO，只读 OS 接口表，<10ms 完成。

use if_addrs::{IfAddr, get_if_addrs};
use std::net::Ipv4Addr;

#[derive(Debug, Clone)]
pub struct HostInterface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

/// 列出本机所有「可能用来路由到外部 IP」的 IPv4 接口。
///
/// 排除：
/// - loopback（127.x）
/// - link-local（169.254.x，DHCP 失败时的兜底地址，不可路由）
/// - unspecified（0.0.0.0）
pub fn list_active_ipv4_interfaces() -> Vec<HostInterface> {
    let Ok(ifaces) = get_if_addrs() else {
        return Vec::new();
    };
    ifaces
        .into_iter()
        .filter_map(|iface| {
            if iface.is_loopback() {
                return None;
            }
            match iface.addr {
                IfAddr::V4(v4) => {
                    if v4.ip.is_link_local() || v4.ip.is_unspecified() {
                        return None;
                    }
                    Some(HostInterface { name: iface.name, ip: v4.ip, netmask: v4.netmask })
                },
                IfAddr::V6(_) => None,
            }
        })
        .collect()
}

/// 给一个目标 IP 做诊断：本机能否直接连上去？
///
/// 返回 Ok(找到的同网段接口) 或 Err(用户可读的失败原因)
pub fn diagnose_for_target(target: Ipv4Addr) -> Result<HostInterface, String> {
    let mut interfaces = list_active_ipv4_interfaces();

    if interfaces.is_empty() {
        return Err(
            "电脑未连接到任何网络（WiFi/有线均不可用），无法切换无线。请先让电脑联网后重试。"
                .to_string(),
        );
    }

    if let Some(idx) =
        interfaces.iter().position(|i| in_same_subnet(i.ip, i.netmask, target))
    {
        return Ok(interfaces.swap_remove(idx));
    }

    let pc_ips = interfaces
        .iter()
        .map(|i| format!("{}({})", i.ip, i.name))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "电脑当前网络（{}）与手机 WiFi（{}）不在同一网段，无法切换。请确认电脑与手机连到同一 WiFi。",
        pc_ips, target
    ))
}

/// `(local & mask) == (target & mask)` 即同子网
fn in_same_subnet(local: Ipv4Addr, mask: Ipv4Addr, target: Ipv4Addr) -> bool {
    let l = u32::from(local);
    let m = u32::from(mask);
    let t = u32::from(target);
    (l & m) == (t & m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn same_subnet_24() {
        assert!(in_same_subnet(ip("192.168.1.5"), ip("255.255.255.0"), ip("192.168.1.50")));
    }

    #[test]
    fn different_subnet_24() {
        assert!(!in_same_subnet(ip("192.168.1.5"), ip("255.255.255.0"), ip("192.168.2.50")));
    }

    #[test]
    fn same_subnet_16_covers_24() {
        // 电脑 /16 子网包含手机所在的 /24
        assert!(in_same_subnet(ip("192.168.0.1"), ip("255.255.0.0"), ip("192.168.1.50")));
    }

    #[test]
    fn different_class_b() {
        assert!(!in_same_subnet(ip("10.0.0.1"), ip("255.255.255.0"), ip("192.168.1.50")));
    }

    #[test]
    fn list_interfaces_runs_without_panic() {
        // 只确保不 panic；具体内容依赖运行环境，不做断言
        let _ = list_active_ipv4_interfaces();
    }
}
