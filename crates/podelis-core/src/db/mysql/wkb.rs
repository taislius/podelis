//! MySQL's spatial values as text: they come as a 4-byte SRID and then
//! the geometry in Well-Known Binary (OGC Simple Features); shown as
//! Well-Known Text, as `ST_AsText` writes it (`POINT(1 2)`).
//!
//! Only with SRID 0 (no reference system, MySQL's default): with another,
//! `ST_AsText` writes the coordinates in the system's axis order (latitude
//! first for 4326), which may not be the order they're kept in, so such a
//! value stays in hex rather than be shown with its axes swapped.

/// The WKT of a MySQL geometry value, or `None` if it isn't one this
/// reads (it's then shown in hex).
pub(super) fn wkt(value: &[u8]) -> Option<String> {
    let (srid, wkb) = value.split_first_chunk::<4>()?;
    if u32::from_le_bytes(*srid) != 0 {
        return None;
    }
    let mut reader = Reader { bytes: wkb, at: 0 };
    let text = reader.geometry(0)?;
    if reader.at != wkb.len() {
        return None;
    }
    Some(text)
}

/// Nested collections deeper than this aren't read (a value made to be
/// deep mustn't take the stack).
const MAX_DEPTH: usize = 32;

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let chunk = self.bytes.get(self.at..self.at + N)?.try_into().ok()?;
        self.at += N;
        Some(chunk)
    }

    fn u32(&mut self, little: bool) -> Option<u32> {
        let b = self.take::<4>()?;
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    fn f64(&mut self, little: bool) -> Option<f64> {
        let b = self.take::<8>()?;
        Some(if little { f64::from_le_bytes(b) } else { f64::from_be_bytes(b) })
    }

    /// A count of what follows, no more than the bytes left could hold
    /// (`min` bytes each): a made-up count mustn't allocate.
    fn count(&mut self, little: bool, min: usize) -> Option<usize> {
        let n = self.u32(little)? as usize;
        (n.saturating_mul(min) <= self.bytes.len() - self.at).then_some(n)
    }

    fn point(&mut self, little: bool) -> Option<String> {
        let (x, y) = (self.f64(little)?, self.f64(little)?);
        Some(format!("{} {}", number(x), number(y)))
    }

    fn points(&mut self, little: bool) -> Option<String> {
        let n = self.count(little, 16)?;
        let points: Option<Vec<String>> = (0..n).map(|_| self.point(little)).collect();
        Some(bracketed(&points?))
    }

    fn rings(&mut self, little: bool) -> Option<String> {
        let n = self.count(little, 4)?;
        let rings: Option<Vec<String>> = (0..n).map(|_| self.points(little)).collect();
        Some(bracketed(&rings?))
    }

    /// One geometry: its byte order, its type, then its coordinates.
    fn geometry(&mut self, depth: usize) -> Option<String> {
        if depth > MAX_DEPTH {
            return None;
        }
        let little = match self.take::<1>()? {
            [1] => true,
            [0] => false,
            _ => return None,
        };
        let kind = self.u32(little)?;
        Some(match kind {
            1 => format!("POINT({})", self.point(little)?),
            2 => format!("LINESTRING{}", self.points(little)?),
            3 => format!("POLYGON{}", self.rings(little)?),
            4..=7 => {
                let name = ["MULTIPOINT", "MULTILINESTRING", "MULTIPOLYGON", "GEOMETRYCOLLECTION"][kind as usize - 4];
                let n = self.count(little, 5)?;
                let parts: Option<Vec<String>> = (0..n)
                    .map(|_| {
                        let part = self.geometry(depth + 1)?;
                        // A multi's parts are written without their type
                        // name (`MULTIPOINT((1 2),(3 4))`); a collection's with.
                        Some(if kind == 7 { part } else { strip_name(&part) })
                    })
                    .collect();
                format!("{name}{}", bracketed(&parts?))
            }
            _ => return None,
        })
    }
}

/// MySQL's own bytes for a WKT value (SRID 0, as `wkt` writes it), for an
/// edited cell: the same bytes it was read from, so no rounding comes
/// between. `None` if it isn't WKT this reads.
pub(super) fn from_wkt(text: &str) -> Option<Vec<u8>> {
    let mut parser = Parser { text: text.trim().as_bytes(), at: 0, out: 0u32.to_le_bytes().to_vec() };
    parser.geometry(0)?;
    parser.space();
    (parser.at == parser.text.len()).then_some(parser.out)
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
    out: Vec<u8>,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.space();
        (self.text.get(self.at) == Some(&c)).then(|| self.at += 1)
    }

    fn peek(&mut self, c: u8) -> bool {
        self.space();
        self.text.get(self.at) == Some(&c)
    }

    fn word(&mut self) -> String {
        self.space();
        let start = self.at;
        while self.text.get(self.at).is_some_and(u8::is_ascii_alphabetic) {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.text[start..self.at]).to_ascii_uppercase()
    }

    fn number(&mut self) -> Option<f64> {
        self.space();
        let start = self.at;
        while self.text.get(self.at).is_some_and(|&b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            self.at += 1;
        }
        std::str::from_utf8(&self.text[start..self.at]).ok()?.parse().ok()
    }

    fn header(&mut self, kind: u32) {
        self.out.push(1);
        self.out.extend(kind.to_le_bytes());
    }

    fn coordinates(&mut self) -> Option<()> {
        let (x, y) = (self.number()?, self.number()?);
        self.out.extend(x.to_le_bytes());
        self.out.extend(y.to_le_bytes());
        Some(())
    }

    /// `(a, b, …)`, each read by `item`, its count first; `EMPTY` for none.
    fn list(&mut self, mut item: impl FnMut(&mut Self) -> Option<()>) -> Option<()> {
        let count_at = self.out.len();
        self.out.extend(0u32.to_le_bytes());
        self.space();
        if self.text.get(self.at..self.at + 5).is_some_and(|w| w.eq_ignore_ascii_case(b"EMPTY")) {
            self.at += 5;
            return Some(());
        }
        self.eat(b'(')?;
        let mut n = 0u32;
        loop {
            item(self)?;
            n += 1;
            if self.eat(b',').is_none() {
                break;
            }
        }
        self.eat(b')')?;
        self.out[count_at..count_at + 4].copy_from_slice(&n.to_le_bytes());
        Some(())
    }

    /// A point of a multipoint: `(1 2)`, or `1 2` as some write it.
    fn bare_or_bracketed_point(&mut self) -> Option<()> {
        self.header(1);
        if self.peek(b'(') {
            self.eat(b'(')?;
            self.coordinates()?;
            self.eat(b')')
        } else {
            self.coordinates()
        }
    }

    fn geometry(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let ring = |p: &mut Self| p.list(Self::coordinates);
        match self.word().as_str() {
            "POINT" => {
                self.header(1);
                self.eat(b'(')?;
                self.coordinates()?;
                self.eat(b')')
            }
            "LINESTRING" => {
                self.header(2);
                self.list(Self::coordinates)
            }
            "POLYGON" => {
                self.header(3);
                self.list(ring)
            }
            "MULTIPOINT" => {
                self.header(4);
                self.list(Self::bare_or_bracketed_point)
            }
            "MULTILINESTRING" => {
                self.header(5);
                self.list(|p| {
                    p.header(2);
                    p.list(Self::coordinates)
                })
            }
            "MULTIPOLYGON" => {
                self.header(6);
                self.list(|p| {
                    p.header(3);
                    p.list(ring)
                })
            }
            "GEOMETRYCOLLECTION" => {
                self.header(7);
                self.list(|p| p.geometry(depth + 1))
            }
            _ => None,
        }
    }
}

/// `(a,b)`, or ` EMPTY` for none, as `ST_AsText` writes them.
fn bracketed(parts: &[String]) -> String {
    if parts.is_empty() { " EMPTY".to_string() } else { format!("({})", parts.join(",")) }
}

/// `POINT(1 2)` → `(1 2)`; `LINESTRING EMPTY` → `EMPTY`.
fn strip_name(part: &str) -> String {
    // An empty part: `MULTILINESTRING(EMPTY,(0 0,1 1))`.
    if part.ends_with(" EMPTY") {
        return "EMPTY".to_string();
    }
    part.find('(').map_or_else(|| part.to_string(), |i| part[i..].to_string())
}

/// A coordinate as `ST_AsText` writes it: `1`, not `1.0`.
fn number(x: f64) -> String {
    if x.fract() == 0.0 && x.abs() < 1e15 { format!("{x:.0}") } else { x.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f64, y: f64) -> Vec<u8> {
        let mut b = vec![1];
        b.extend(1u32.to_le_bytes());
        b.extend(x.to_le_bytes());
        b.extend(y.to_le_bytes());
        b
    }

    fn with_srid(srid: u32, wkb: &[u8]) -> Vec<u8> {
        let mut b = srid.to_le_bytes().to_vec();
        b.extend(wkb);
        b
    }

    #[test]
    fn points_lines_and_collections_read_as_st_astext_writes_them() {
        assert_eq!(wkt(&with_srid(0, &point(1.0, 2.5))).as_deref(), Some("POINT(1 2.5)"));
        // Another reference system: its axis order isn't known here.
        assert_eq!(wkt(&with_srid(4326, &point(25.28, 54.69))), None);
        let mut line = vec![1];
        line.extend(2u32.to_le_bytes());
        line.extend(2u32.to_le_bytes());
        for c in [0.0f64, 0.0, 3.0, 4.0] {
            line.extend(c.to_le_bytes());
        }
        assert_eq!(wkt(&with_srid(0, &line)).as_deref(), Some("LINESTRING(0 0,3 4)"));
        let mut multi = vec![1];
        multi.extend(4u32.to_le_bytes());
        multi.extend(2u32.to_le_bytes());
        multi.extend(point(1.0, 2.0));
        multi.extend(point(3.0, 4.0));
        assert_eq!(wkt(&with_srid(0, &multi)).as_deref(), Some("MULTIPOINT((1 2),(3 4))"));
        let mut collection = vec![1];
        collection.extend(7u32.to_le_bytes());
        collection.extend(1u32.to_le_bytes());
        collection.extend(point(1.0, 2.0));
        assert_eq!(wkt(&with_srid(0, &collection)).as_deref(), Some("GEOMETRYCOLLECTION(POINT(1 2))"));
    }

    #[test]
    fn wkt_written_back_is_the_bytes_it_was_read_from() {
        for text in [
            "POINT(1 2.5)",
            "LINESTRING(0 0,3 4)",
            "POLYGON((0 0,4 0,4 4,0 0),(1 1,2 1,1 2,1 1))",
            "MULTIPOINT((1 2),(3 4))",
            "MULTILINESTRING((0 0,1 1),(2 2,3 3))",
            "MULTIPOLYGON(((0 0,1 0,1 1,0 0)))",
            "GEOMETRYCOLLECTION(POINT(1 2),LINESTRING(0 0,1 1))",
            "POINT(0.1 -0.0000001)",
            "GEOMETRYCOLLECTION EMPTY",
            "LINESTRING EMPTY",
            "MULTIPOLYGON EMPTY",
            "MULTILINESTRING(EMPTY,(0 0,1 1))",
        ] {
            let bytes = from_wkt(text).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(wkt(&bytes).as_deref(), Some(text));
        }
        assert_eq!(from_wkt(" point ( 1  2 ) "), from_wkt("POINT(1 2)"));
        assert_eq!(from_wkt("MULTIPOINT(1 2, 3 4)"), from_wkt("MULTIPOINT((1 2),(3 4))"));
        assert_eq!(from_wkt("POINT(0.1 -1e-7)"), from_wkt("POINT(0.1 -0.0000001)"));
        assert_eq!(from_wkt("POINT(1)"), None);
        assert_eq!(from_wkt("POINT(1 2) x"), None);
        assert_eq!(from_wkt("CIRCLE(1 2)"), None);
    }

    #[test]
    fn an_edited_geometry_is_its_bytes_and_a_blob_stays_text() {
        use crate::db::{Engine, value_literal};
        let point = value_literal("POINT(1 2)", "GEOMETRY", Engine::MySql);
        assert!(point.starts_with("0x00000000010100000000"), "{point}");
        assert_eq!(value_literal("POINT(1 2)", "BLOB", Engine::MySql), "'POINT(1 2)'");
        assert_eq!(value_literal("0x00FF", "BLOB", Engine::MySql), "0x00FF");
    }

    #[test]
    fn what_isnt_a_geometry_isnt_read() {
        assert_eq!(wkt(b"\x00\xff"), None);
        // A count far past the bytes there are.
        let mut line = vec![0, 0, 0, 0, 1];
        line.extend(2u32.to_le_bytes());
        line.extend(u32::MAX.to_le_bytes());
        assert_eq!(wkt(&line), None);
        // Bytes left over.
        let mut extra = with_srid(0, &point(1.0, 2.0));
        extra.push(0);
        assert_eq!(wkt(&extra), None);
    }
}
