//! CanelaRemote: captura de UNA pantalla para canela_cmd.rs (parche 33), con el mismo scrap que
//! usa el video de las sesiones. Va aparte porque scrap no compila en la prueba (canela/cmd-check),
//! que usa una captura de mentira.

use crate::canela_cmd::{to_rgba, Shot};
use scrap::{Frame, TraitCapturer, TraitPixelBuffer};
use std::time::{Duration, Instant};

/// Cuánto se espera el primer cuadro (en Windows, DXGI puede tardar en darlo).
const FIRST_FRAME: Duration = Duration::from_secs(5);

pub fn displays() -> usize {
    crate::server::display_service::try_get_displays()
        .map(|d| d.len())
        .unwrap_or(0)
}

pub fn capture(idx: usize) -> Result<Shot, String> {
    // la pantalla de bloqueo / UAC es otro escritorio: hay que pasarse a él para verla
    #[cfg(windows)]
    crate::platform::try_change_desktop();
    let mut all = crate::server::display_service::try_get_displays()
        .map_err(|e| format!("no se pudo leer las pantallas: {e}"))?;
    let n = all.len();
    if idx >= n {
        return Err(format!("no hay pantalla {idx}: este equipo tiene {n} (se cuentan desde 0)"));
    }
    let display = all.swap_remove(idx);
    let mut c = scrap::Capturer::new(display).map_err(|e| format!("no se pudo capturar la pantalla: {e}"))?;
    let deadline = Instant::now() + FIRST_FRAME;
    #[cfg(windows)]
    let mut tried_gdi = false;
    loop {
        match c.frame(Duration::from_millis(300)) {
            Ok(Frame::PixelBuffer(f)) => {
                let (w, h) = (f.width(), f.height());
                let stride = f.stride().first().copied().unwrap_or(w * 4);
                let bgra = matches!(f.pixfmt(), scrap::Pixfmt::BGRA);
                if !bgra && !matches!(f.pixfmt(), scrap::Pixfmt::RGBA) {
                    return Err("la pantalla vino en un formato de color que no se maneja".into());
                }
                let rgba = to_rgba(f.data(), w, h, stride, bgra)?;
                return Ok(Shot { width: w as u32, height: h as u32, rgba, displays: n });
            }
            Ok(Frame::Texture(_)) => return Err("la pantalla vino como textura de video; prueba otra vez".into()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => {
                // DXGI ocupado o sin permiso: GDI es más lento pero casi siempre sirve
                #[cfg(windows)]
                if !tried_gdi {
                    tried_gdi = true;
                    if c.set_gdi() {
                        continue;
                    }
                }
                return Err(format!("no se pudo capturar la pantalla: {e}"));
            }
        }
        if Instant::now() > deadline {
            return Err("la pantalla no mandó imagen en 5 s (¿apagada o bloqueada?)".into());
        }
    }
}
