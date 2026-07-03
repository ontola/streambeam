//! Terminal QR rendering for the pairing payload.

use std::io::Write;

use qrcode::QrCode;

pub fn print_terminal(payload: &str) {
    let Ok(code) = QrCode::new(payload.as_bytes()) else {
        eprintln!("(could not render QR — paste the sbm1: line manually)");
        return;
    };
    let string = code
        .render::<char>()
        .quiet_zone(false)
        .module_dimensions(1, 2)
        .build();
    let _ = writeln!(std::io::stderr(), "{string}");
}
