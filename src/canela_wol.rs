//! CanelaRemote: encender equipos a distancia (Wake-on-LAN, parche 30).
//!
//! Una PC apagada solo despierta con un "paquete mágico" (6 bytes 0xFF + su MAC 16 veces) que le
//! llegue por SU red local: desde internet no entra. Por eso esto tiene dos partes:
//!
//! * **Reporte**: cada agente le dice a la API sus tarjetas de red de área local (MAC, IPv4,
//!   prefijo y si es por cable) en `POST /api/canela/nics`. Así la API sabe a qué MAC hay que
//!   despertar y qué otros equipos están en la misma red.
//! * **Despertar**: cuando un técnico pide "Encender", la API le deja el pedido a equipos
//!   encendidos de esa red, que lo reciben en su latido (`canela.wol = [{mac, bcast}]`, lo engancha
//!   `hbbs_http/sync.rs`) y mandan el paquete por broadcast desde cada una de sus tarjetas.
//!
//! Seguridad: el reporte no lleva nada secreto, y el despertar solo manda paquetes mágicos (UDP
//! 9 y 7) a la red local: no ejecuta nada ni abre puertos. Se apaga con `canela-wol=N` en custom.txt.

#![allow(dead_code)]
use hbb_common::tokio; // tokio re-exportado por hbb_common (features full): para #[tokio::main]
use serde_json::{json, Value};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

/// Tarjeta de red de área local del equipo.
#[derive(Debug, Clone, PartialEq)]
pub struct Nic {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    pub prefix: u8,
    pub wired: bool,
}

/// Máximo de MACs por pedido (la API manda pocas; esto es por si llega basura).
pub const MAX_TARGETS: usize = 16;
/// Puertos donde escuchan las tarjetas con Wake-on-LAN (el 9 es el estándar; el 7 por si acaso).
pub const WOL_PORTS: [u16; 2] = [9, 7];
/// Rondas de envío por pedido: el UDP se puede perder y no hay confirmación.
pub const ROUNDS: usize = 3;

// ─────────────────────────── Lógica pura (probada en CI) ───────────────────────────

/// "aa:bb:cc:dd:ee:ff" o "AA-BB-…" → bytes. Nula, broadcast o multicast → None.
pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.trim().split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        if p.len() != 2 {
            return None;
        }
        mac[i] = u8::from_str_radix(p, 16).ok()?;
    }
    if mac == [0; 6] || mac == [0xff; 6] || mac[0] & 1 == 1 {
        return None;
    }
    Some(mac)
}

pub fn fmt_mac(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// El paquete mágico: 6 × 0xFF y la MAC 16 veces (102 bytes).
pub fn magic_packet(mac: &[u8; 6]) -> [u8; 102] {
    let mut p = [0xffu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(mac);
    }
    p
}

/// Broadcast de la subred: 192.168.1.20/24 → 192.168.1.255.
pub fn broadcast_of(ip: Ipv4Addr, prefix: u8) -> Ipv4Addr {
    let mask: u32 = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32) as u32) };
    Ipv4Addr::from(u32::from(ip) | !mask)
}

/// IPv4 de red local donde un broadcast tiene sentido (privada o CGNAT; ni loopback ni APIPA).
pub fn is_lan(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_private() || (o[0] == 100 && (64..=127).contains(&o[1]))
}

/// Adaptadores virtuales (máquinas virtuales, VPN, Docker): su MAC no despierta ninguna PC.
pub fn is_virtual(label: &str) -> bool {
    const VIRTUAL: &[&str] = &[
        "vethernet", "hyper-v", "vmware", "virtualbox", "vbox", "tap-", "tap0", "tun", "docker",
        "veth", "virbr", "br-", "wireguard", "wg", "tailscale", "zerotier", "hamachi", "loopback",
        "bluetooth", "npcap", "utun", "awdl", "llw", "bridge", "vpn", "fortinet", "openvpn",
    ];
    let l = label.to_ascii_lowercase();
    VIRTUAL.iter().any(|v| l.contains(v))
}

/// Lo que se reporta a la API.
pub fn nics_json(nics: &[Nic]) -> Value {
    Value::Array(
        nics.iter()
            .map(|n| json!({ "mac": fmt_mac(&n.mac), "ip": n.ip.to_string(), "prefix": n.prefix, "wired": n.wired }))
            .collect(),
    )
}

/// `canela.wol` del latido → [(mac, broadcast de su subred si vino)]. Ignora lo mal formado.
pub fn parse_targets(v: &Value) -> Vec<([u8; 6], Option<Ipv4Addr>)> {
    let mut out = vec![];
    for t in v.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let Some(mac) = t.get("mac").and_then(|m| m.as_str()).and_then(parse_mac) else {
            continue;
        };
        let bcast = t.get("bcast").and_then(|b| b.as_str()).and_then(|b| b.parse::<Ipv4Addr>().ok());
        if !out.iter().any(|(m, _)| *m == mac) {
            out.push((mac, bcast));
        }
        if out.len() >= MAX_TARGETS {
            break;
        }
    }
    out
}

/// A dónde mandar desde una tarjeta local: broadcast general, el de su subred y el que pidió la
/// API (si es otro: la PC apagada puede estar en otra subred alcanzable por broadcast dirigido).
pub fn destinations(local: &Nic, bcast: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut d = vec![Ipv4Addr::BROADCAST, broadcast_of(local.ip, local.prefix)];
    if let Some(b) = bcast {
        if !d.contains(&b) {
            d.push(b);
        }
    }
    d
}

// ─────────────────────────── Tarjetas del equipo ───────────────────────────

/// Tarjetas de red de área local con MAC real (sin virtuales ni loopback).
#[cfg(not(target_os = "ios"))]
pub fn detect() -> Vec<Nic> {
    let mut out = vec![];
    for i in default_net::get_interfaces() {
        let label = format!(
            "{} {} {}",
            i.name,
            i.friendly_name.clone().unwrap_or_default(),
            i.description.clone().unwrap_or_default()
        );
        if is_virtual(&label) {
            continue;
        }
        let Some(mac) = i.mac_addr.as_ref().and_then(|m| parse_mac(&m.address())) else {
            continue;
        };
        let wired = matches!(i.if_type, default_net::interface::InterfaceType::Ethernet);
        for v4 in &i.ipv4 {
            if is_lan(v4.addr) && (8..=30).contains(&v4.prefix_len) {
                out.push(Nic { mac, ip: v4.addr, prefix: v4.prefix_len, wired });
            }
        }
    }
    out.truncate(8);
    out
}

#[cfg(target_os = "ios")]
pub fn detect() -> Vec<Nic> {
    vec![]
}

// ─────────────────────────── Despertar (lo pide la API en el latido) ───────────────────────────

/// Manda los paquetes en un hilo aparte (el latido no espera).
pub fn wake(v: &Value) {
    if disabled() {
        return;
    }
    let targets = parse_targets(v);
    if targets.is_empty() {
        return;
    }
    std::thread::spawn(move || send_all(&targets));
}

fn send_all(targets: &[([u8; 6], Option<Ipv4Addr>)]) {
    let locals = detect();
    for (mac, bcast) in targets {
        let pkt = magic_packet(mac);
        let mut sent = 0;
        for round in 0..ROUNDS {
            if round > 0 {
                std::thread::sleep(std::time::Duration::from_millis(400));
            }
            for local in &locals {
                sent += send_from(local.ip, &destinations(local, *bcast), &pkt);
            }
            if locals.is_empty() {
                // sin tarjetas detectadas: que el sistema escoja por dónde sale
                let mut d = vec![Ipv4Addr::BROADCAST];
                d.extend(bcast.iter().copied());
                sent += send_from(Ipv4Addr::UNSPECIFIED, &d, &pkt);
            }
        }
        hbb_common::log::info!("canela: encender {} → {sent} paquete(s) mágico(s) enviado(s)", fmt_mac(mac));
    }
}

/// Envía `pkt` desde la IP local `from` a cada destino y puerto; devuelve cuántos salieron.
fn send_from(from: Ipv4Addr, dests: &[Ipv4Addr], pkt: &[u8]) -> usize {
    let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(from, 0)) else {
        return 0;
    };
    if sock.set_broadcast(true).is_err() {
        return 0;
    }
    let mut n = 0;
    for d in dests {
        for port in WOL_PORTS {
            if sock.send_to(pkt, SocketAddrV4::new(*d, port)).is_ok() {
                n += 1;
            }
        }
    }
    n
}

// ─────────────────────────── Reporte a la API ───────────────────────────

/// ¿Está apagado por configuración? (`canela-wol=N`)
fn disabled() -> bool {
    hbb_common::config::Config::get_option("canela-wol").eq_ignore_ascii_case("N")
}

/// Reporta las tarjetas si cambiaron (o si `force`). Guarda un hash para no repetir el POST.
pub async fn report(force: bool) {
    if disabled() {
        return;
    }
    let api = crate::get_api_server(
        hbb_common::config::Config::get_option("api-server"),
        hbb_common::config::Config::get_option("custom-rendezvous-server"),
    );
    if api.is_empty() {
        return;
    }
    let nics = detect();
    let arr = nics_json(&nics);
    let hash = format!("{:x}", hash_of(&arr.to_string()));
    if !force && hash == hbb_common::config::Status::get("canela_nics_hash") {
        return; // sin cambios: no repetir
    }
    let body = json!({ "id": hbb_common::config::Config::get_id(), "nics": arr }).to_string();
    match crate::post_request(format!("{api}/api/canela/nics"), body, "").await {
        Ok(_) => {
            hbb_common::config::Status::set("canela_nics_hash", hash);
            hbb_common::log::info!("canela: {} tarjeta(s) de red reportada(s)", nics.len());
        }
        Err(e) => hbb_common::log::warn!("canela: no se pudo reportar las tarjetas de red: {e}"),
    }
}

fn hash_of(s: &str) -> u128 {
    // hash simple y estable (no criptográfico): solo para saber si cambió el reporte.
    let mut h: u128 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = h.wrapping_mul(0x1000_0000_01b3).wrapping_add(b as u128);
    }
    h
}

use std::sync::atomic::{AtomicBool, Ordering};
static STARTED: AtomicBool = AtomicBool::new(false);

/// Arranca el reporte periódico (una sola vez): a los 40 s, cada 30 min si cambió (la IP por DHCP
/// puede cambiar) y cada 6 h aunque no cambie (por si la API lo perdió). Lo llama `sync::start()`.
pub fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(40)); // dejar que el agente arranque
        let mut n: u32 = 0;
        loop {
            report_blocking(n % 12 == 0);
            n = n.wrapping_add(1);
            std::thread::sleep(std::time::Duration::from_secs(30 * 60));
        }
    });
}

#[tokio::main(flavor = "current_thread")]
async fn report_blocking(force: bool) {
    report(force).await;
}
