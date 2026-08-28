//! Bind 4433, run candidate gathering against the real network, print the result.
//!
//!     cargo run -p seyd-nat --example gather

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let socks = seyd_nat::bind_sockets(4433, true).expect("bind");
    let opts = seyd_nat::GatherOpts {
        port: 4433,
        host_override: None,
        port_mapping: true,
        ipv6: true,
    };
    let g = seyd_nat::gather(&socks, &opts).await;
    println!("candidates:");
    for c in &g.candidates {
        println!(
            "  [{:<8} prio {:>3} probe={}] {}",
            c.label, c.priority, c.needs_probe, c.url
        );
    }
    println!("p2p_hint: {:?}", g.p2p_hint);
    println!(
        "nat_report: {}",
        serde_json::to_string_pretty(&g.nat_report).unwrap()
    );
}
