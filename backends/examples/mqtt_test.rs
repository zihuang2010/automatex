use rumqttc::{AsyncClient, Event, Incoming, MqttOptions};
use std::time::Duration;

#[tokio::main]
async fn main() {
    // 测试 1: 无认证
    println!("=== 测试1: 无认证 ===");
    test("test-no-auth", None, None).await;

    // 测试 2: 有认证
    println!("=== 测试2: 有认证 ===");
    test("automatex-18591", Some("automatex"), Some("zihuang2010=-0")).await;

    // 测试 3: 有认证 + 短 client_id
    println!("=== 测试3: 短 client_id ===");
    test("atx-test", Some("automatex"), Some("zihuang2010=-0")).await;
}

async fn test(client_id: &str, user: Option<&str>, pass: Option<&str>) {
    let mut opts = MqttOptions::new(client_id, "39.98.170.208", 30002);
    opts.set_keep_alive(Duration::from_secs(30));
    opts.set_clean_session(true);
    if let (Some(u), Some(p)) = (user, pass) {
        opts.set_credentials(u, p);
    }

    let (_client, mut eventloop) = AsyncClient::new(opts, 10);

    let r = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(ack))) => {
                    println!("  ✅ ConnAck: {:?}", ack);
                    return "OK";
                },
                Ok(Event::Outgoing(_)) => continue,
                Ok(ev) => {
                    println!("  event: {:?}", ev);
                },
                Err(e) => {
                    println!("  ❌ {}", e);
                    return "FAIL";
                },
            }
        }
    })
    .await;

    match r {
        Ok(s) => println!("  → {}\n", s),
        Err(_) => println!("  → TIMEOUT\n"),
    }
}
