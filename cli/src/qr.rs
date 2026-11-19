//! QR codes drawn in a terminal, so a phone can scan what the CLI prints (an organisation enrolment token, ADR 0035).
//!
//! Each character cell holds two modules, one above the other, using the half-block characters. The code is
//! drawn black on white with explicit colours, whatever the terminal's theme, because many scanners do not
//! read a light-on-dark code.

use anyhow::{anyhow, Result};
use qrcode::{Color, EcLevel, QrCode};

/// Light modules around the code; the QR standard asks for four.
pub const QUIET: usize = 4;

const BLACK_ON_WHITE: &str = "\x1b[30;107m";
const RESET: &str = "\x1b[0m";

/// The code's modules, row by row, `true` for dark, without the quiet zone.
pub fn matrix(text: &str) -> Result<Vec<Vec<bool>>> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::L)
        .map_err(|e| anyhow!("that does not fit in a QR code: {e}"))?;
    let w = code.width();
    Ok(code
        .to_colors()
        .chunks(w)
        .map(|r| r.iter().map(|c| *c == Color::Dark).collect())
        .collect())
}

/// `text` as a QR code for a terminal, one line per two rows of modules, quiet zone included.
pub fn terminal(text: &str) -> Result<String> {
    let m = matrix(text)?;
    let n = m.len();
    let side = n + 2 * QUIET;
    let dark = |y: usize, x: usize| -> bool {
        y >= QUIET && x >= QUIET && y < QUIET + n && x < QUIET + n && m[y - QUIET][x - QUIET]
    };
    let mut out = String::new();
    for y in (0..side).step_by(2) {
        out.push_str(BLACK_ON_WHITE);
        for x in 0..side {
            out.push(match (dark(y, x), dark(y + 1, x)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push_str(RESET);
        out.push('\n');
    }
    Ok(out)
}

/// Reads [`terminal`]'s drawing back into modules (quiet zone removed), for tests and for drawing the same
/// code as an image.
pub fn read_terminal(drawn: &str) -> Vec<Vec<bool>> {
    let mut rows: Vec<Vec<bool>> = Vec::new();
    for line in drawn.lines() {
        let cells: String = line.replace(BLACK_ON_WHITE, "").replace(RESET, "");
        let (mut top, mut bottom) = (Vec::new(), Vec::new());
        for c in cells.chars() {
            let (t, b) = match c {
                '█' => (true, true),
                '▀' => (true, false),
                '▄' => (false, true),
                _ => (false, false),
            };
            top.push(t);
            bottom.push(b);
        }
        rows.push(top);
        rows.push(bottom);
    }
    let side = rows.first().map_or(0, Vec::len);
    let n = side.saturating_sub(2 * QUIET);
    rows.iter()
        .skip(QUIET)
        .take(n)
        .map(|r| r[QUIET..QUIET + n].to_vec())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_drawing_is_the_code() {
        let text = "spikeorg1:eyJ0ZW5hbnQiOiJub3J0aHdpbmQifQ";
        let drawn = terminal(text).unwrap();
        assert_eq!(read_terminal(&drawn), matrix(text).unwrap());
        // Every line is the same width: the code plus its quiet zone, on a white background.
        let n = matrix(text).unwrap().len();
        for line in drawn.lines() {
            assert!(line.starts_with(BLACK_ON_WHITE) && line.ends_with(RESET));
            let cells = line.replace(BLACK_ON_WHITE, "").replace(RESET, "");
            assert_eq!(cells.chars().count(), n + 2 * QUIET);
        }
    }
}
