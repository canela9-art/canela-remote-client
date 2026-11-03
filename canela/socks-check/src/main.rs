// Prueba de src/canela_socks.rs sin RustDesk: marcos, SOCKS5 y, de punta a punta, un programa
// que entra por el proxy local, pasa por el multiplexor (la "sesión" es un par TCP local) y llega
// a un servidor eco que hace de equipo de la red del cliente.
#[path = "../../../src/canela_socks.rs"]
mod canela_socks;

use canela_socks::*;
use hbb_common::tokio::{
    self,
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

fn frames() {
    let f = Frame { kind: DATA, id: 7, payload: b"hola".to_vec() };
    let bytes = encode(&f);
    assert_eq!(decode(&bytes).unwrap(), Some((f.clone(), bytes.len())));
    assert_eq!(decode(&bytes[..5]).unwrap(), None); // falta cabecera
    assert_eq!(decode(&bytes[..bytes.len() - 1]).unwrap(), None); // faltan datos
    let mut two = bytes.clone();
    two.extend(encode(&Frame { kind: CLOSE, id: 7, payload: vec![] }));
    let (_, used) = decode(&two).unwrap().unwrap();
    assert_eq!(decode(&two[used..]).unwrap().unwrap().0.kind, CLOSE);
    assert!(decode(&[9, 0, 0, 0, 1, 0, 0, 0, 0]).is_err()); // tipo desconocido
    assert!(decode(&[DATA, 0, 0, 0, 1, 0xff, 0xff, 0xff, 0xff]).is_err()); // largo absurdo
    assert_eq!(parse_open(&open_payload("192.168.1.1", 80)), Some(("192.168.1.1".into(), 80)));
    assert_eq!(parse_open(&open_payload("", 80)), None);
    assert_eq!(parse_open(&open_payload("x", 0)), None);
    assert_eq!(parse_err(&err_payload(REP_REFUSED, "rechazada")), (REP_REFUSED, "rechazada".into()));
    assert_eq!(parse_addr(1, &[192, 168, 1, 50]), Some("192.168.1.50".into()));
    assert_eq!(parse_addr(3, b"servidor"), Some("servidor".into()));
    assert_eq!(parse_addr(4, &[0; 16]), Some("::".into()));
    assert_eq!(parse_addr(1, &[1, 2, 3]), None);
    assert_eq!(rep_for(std::io::ErrorKind::ConnectionRefused), REP_REFUSED);
    println!("marcos/socks   → ok");
}

/// Servidor eco: hace de impresora/router de la red del cliente.
async fn echo_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

/// Proxy del técnico sobre una sesión simulada (par TCP local) con el equipo del cliente.
async fn proxy() -> u16 {
    let near = serve_local().await.unwrap(); // equipo del cliente
    let mux = MuxClient::start(near); // App Técnico
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            mux.handle_socks(s);
        }
    });
    port
}

/// Programa que usa el proxy: SOCKS5 sin autenticación, CONNECT a (host, puerto).
async fn socks_connect(proxy: u16, atyp: u8, addr: &[u8], port: u16) -> (TcpStream, u8) {
    let mut s = TcpStream::connect(("127.0.0.1", proxy)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.unwrap();
    assert_eq!(r, [5, 0]);
    let mut req = vec![5, 1, 0, atyp];
    if atyp == 3 {
        req.push(addr.len() as u8);
    }
    req.extend_from_slice(addr);
    req.extend_from_slice(&port.to_be_bytes());
    s.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    (s, rep[1])
}

#[tokio::main]
async fn main() {
    frames();
    let echo = echo_server().await;
    let px = proxy().await;

    // IP y nombre de la red del cliente
    let (mut s, rep) = socks_connect(px, 1, &[127, 0, 0, 1], echo).await;
    assert_eq!(rep, REP_OK);
    s.write_all(b"hola").await.unwrap();
    let mut got = [0u8; 4];
    s.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"hola");
    let (mut s2, rep) = socks_connect(px, 3, b"localhost", echo).await;
    assert_eq!(rep, REP_OK);
    s2.write_all(b"x").await.unwrap();
    s2.read_exact(&mut got[..1]).await.unwrap();
    println!("ip y nombre    → ok");

    // mucho dato y muchas conexiones a la vez por la misma sesión
    let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let mut tasks = vec![];
    for _ in 0..20 {
        let big = big.clone();
        tasks.push(tokio::spawn(async move {
            let (s, rep) = socks_connect(px, 1, &[127, 0, 0, 1], echo).await;
            assert_eq!(rep, REP_OK);
            let (mut r, mut w) = s.into_split();
            let data = big.clone();
            let writer = tokio::spawn(async move { w.write_all(&data).await.unwrap(); w });
            let mut back = vec![0u8; big.len()];
            r.read_exact(&mut back).await.unwrap();
            let _w = writer.await.unwrap();
            assert_eq!(back, big);
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    println!("20 × 300 KB    → ok");

    // puerto cerrado en la red del cliente → el programa recibe "rechazada"
    let closed = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };
    let (_s, rep) = socks_connect(px, 1, &[127, 0, 0, 1], closed).await;
    assert_eq!(rep, REP_REFUSED);
    // UDP ASSOCIATE no se soporta
    let mut s = TcpStream::connect(("127.0.0.1", px)).await.unwrap();
    s.write_all(&[5, 1, 0]).await.unwrap();
    let mut r = [0u8; 2];
    s.read_exact(&mut r).await.unwrap();
    s.write_all(&[5, 3, 0, 1, 127, 0, 0, 1, 0, 53]).await.unwrap();
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], REP_CMD_UNSUPPORTED);
    println!("errores        → ok");
    println!("OK: canela_socks");
}
