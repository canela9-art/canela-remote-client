// Prueba aislada de src/canela_wol.rs (encender equipos a distancia): MACs, paquete mágico,
// broadcast de la subred, qué adaptadores se ignoran, lo que se reporta a la API y cómo se lee el
// pedido canela.wol del latido. La detección real corre al final solo para ver que no truene.
#[path = "../../../src/canela_wol.rs"]
mod canela_wol;
pub use hbb_common::ResultType;
pub fn get_api_server(a: String, _c: String) -> String { a }
pub async fn post_request(_u: String, _b: String, _h: &str) -> ResultType<String> { Ok(String::new()) }

use canela_wol::*;
use serde_json::json;
use std::net::Ipv4Addr;

fn main() {
    // MACs
    assert_eq!(parse_mac("AA-BB-CC-DD-EE-0F"), Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x0f]));
    assert_eq!(parse_mac("aa:bb:cc:dd:ee:0f"), Some([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x0f]));
    assert_eq!(parse_mac("00:00:00:00:00:00"), None);
    assert_eq!(parse_mac("ff:ff:ff:ff:ff:ff"), None);
    assert_eq!(parse_mac("01:00:5e:00:00:01"), None); // multicast
    assert_eq!(parse_mac("aa:bb:cc"), None);
    assert_eq!(parse_mac("aa:bb:cc:dd:ee:zz"), None);
    assert_eq!(fmt_mac(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x0f]), "aa:bb:cc:dd:ee:0f");
    println!("mac            → ok");

    // paquete mágico: 6 × FF y la MAC 16 veces
    let mac = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
    let p = magic_packet(&mac);
    assert_eq!(p.len(), 102);
    assert!(p[..6].iter().all(|b| *b == 0xff));
    for i in 0..16 {
        assert_eq!(&p[6 + i * 6..12 + i * 6], &mac);
    }
    println!("paquete mágico → ok");

    // broadcast y red local
    assert_eq!(broadcast_of(Ipv4Addr::new(192, 168, 1, 20), 24), Ipv4Addr::new(192, 168, 1, 255));
    assert_eq!(broadcast_of(Ipv4Addr::new(10, 0, 5, 9), 16), Ipv4Addr::new(10, 0, 255, 255));
    assert_eq!(broadcast_of(Ipv4Addr::new(172, 16, 3, 130), 25), Ipv4Addr::new(172, 16, 3, 255));
    assert!(is_lan(Ipv4Addr::new(192, 168, 0, 5)));
    assert!(is_lan(Ipv4Addr::new(100, 70, 1, 1))); // CGNAT
    assert!(!is_lan(Ipv4Addr::new(8, 8, 8, 8)));
    assert!(!is_lan(Ipv4Addr::new(169, 254, 3, 3)));
    assert!(!is_lan(Ipv4Addr::new(127, 0, 0, 1)));
    println!("broadcast      → ok");

    // adaptadores virtuales fuera, los reales no
    for v in ["vEthernet (Default Switch)", "VMware Virtual Ethernet Adapter for VMnet8", "VirtualBox Host-Only Ethernet Adapter",
              "TAP-Windows Adapter V9", "docker0", "Tailscale Tunnel", "ZeroTier One", "utun3", "Bluetooth Device (Personal Area Network)"] {
        assert!(is_virtual(v), "debió ser virtual: {v}");
    }
    for r in ["Ethernet Realtek PCIe GbE Family Controller", "Wi-Fi Intel(R) Wi-Fi 6 AX201 160MHz", "eth0", "en0",
              "Ethernet 2 Intel(R) Ethernet Connection (7) I219-V", "enp3s0"] {
        assert!(!is_virtual(r), "no debió ser virtual: {r}");
    }
    println!("virtuales      → ok");

    // reporte
    let nics = vec![Nic { mac, ip: Ipv4Addr::new(192, 168, 1, 20), prefix: 24, wired: true }];
    assert_eq!(nics_json(&nics), json!([{ "mac": "10:20:30:40:50:60", "ip": "192.168.1.20", "prefix": 24, "wired": true }]));
    println!("reporte        → {}", nics_json(&nics));

    // pedido del latido
    let t = parse_targets(&json!([
        { "mac": "AA:BB:CC:DD:EE:01", "bcast": "192.168.1.255" },
        { "mac": "aa:bb:cc:dd:ee:01", "bcast": "10.0.0.255" }, // repetida
        { "mac": "aa:bb:cc:dd:ee:02" },                         // sin broadcast
        { "mac": "basura" }, { "bcast": "1.2.3.4" }, "x",
    ]));
    assert_eq!(t, vec![
        ([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01], Some(Ipv4Addr::new(192, 168, 1, 255))),
        ([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x02], None),
    ]);
    assert!(parse_targets(&json!({ "no": "es arreglo" })).is_empty());
    let many: Vec<_> = (0..40).map(|i| json!({ "mac": format!("aa:bb:cc:dd:ee:{:02x}", i * 2) })).collect();
    assert_eq!(parse_targets(&json!(many)).len(), MAX_TARGETS);
    println!("pedido wol     → ok");

    // destinos desde una tarjeta local
    let local = &nics[0];
    assert_eq!(destinations(local, Some(Ipv4Addr::new(192, 168, 1, 255))),
        vec![Ipv4Addr::BROADCAST, Ipv4Addr::new(192, 168, 1, 255)]);
    assert_eq!(destinations(local, Some(Ipv4Addr::new(192, 168, 2, 255))),
        vec![Ipv4Addr::BROADCAST, Ipv4Addr::new(192, 168, 1, 255), Ipv4Addr::new(192, 168, 2, 255)]);
    println!("destinos       → ok");

    // detección real en el runner (solo que no truene)
    println!("tarjetas aquí  → {}", nics_json(&detect()));
    println!("OK: canela_wol");
}
