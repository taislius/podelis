//! The arguments of a mongosh call as BSON: relaxed JSON, as mongosh takes
//! it. Keys may go unquoted (`{age: {$gt: 30}}`), strings in either quotes,
//! a trailing comma is fine, and the shell's helpers make their types:
//! `ObjectId("…")`, `ISODate("…")`, `new Date("…")`, `NumberLong(…)`,
//! `NumberInt(…)`, `NumberDecimal("…")`.

use anyhow::{Result, anyhow, bail};
use ::mongodb::bson::{Bson, Document};

/// The comma-separated values in `text` (a call's arguments).
pub fn values(text: &str) -> Result<Vec<Bson>> {
    let mut p = Reader { s: text.as_bytes(), text, i: 0 };
    let mut out = Vec::new();
    p.blank();
    while p.i < p.s.len() {
        out.push(p.value()?);
        p.blank();
        if p.eat(b',') {
            p.blank();
        } else if p.i < p.s.len() {
            bail!("expected `,` between arguments at {}", p.near());
        }
    }
    Ok(out)
}

/// A value as a document, or why it isn't one.
pub fn document(value: Option<&Bson>, what: &str) -> Result<Document> {
    match value {
        None => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(other) => bail!("{what} should be a document ({{…}}), not {other}"),
    }
}

struct Reader<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Reader<'_> {
    /// What follows the reading point, for messages.
    fn near(&self) -> String {
        let rest: String = self.text[self.i..].chars().take(20).collect();
        if rest.is_empty() { "the end".into() } else { format!("`{rest}`") }
    }

    fn blank(&mut self) {
        loop {
            while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == 0x0b) {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else if self.s[self.i..].starts_with(b"/*") {
                let end = self.text[self.i + 2..].find("*/").map_or(self.s.len(), |e| self.i + 2 + e + 2);
                self.i = end;
            } else {
                return;
            }
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        let found = self.s.get(self.i) == Some(&c);
        if found {
            self.i += 1;
        }
        found
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        self.blank();
        if self.eat(c) { Ok(()) } else { bail!("expected `{}` at {}", c as char, self.near()) }
    }

    fn value(&mut self) -> Result<Bson> {
        self.blank();
        match self.s.get(self.i) {
            None => bail!("a value is missing at the end"),
            Some(b'{') => self.object().map(Bson::Document),
            Some(b'[') => self.array(),
            Some(b'"' | b'\'') => self.string().map(Bson::String),
            Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'.' => self.number(),
            Some(_) => self.word_value(),
        }
    }

    fn object(&mut self) -> Result<Document> {
        self.i += 1;
        let mut doc = Document::new();
        loop {
            self.blank();
            if self.eat(b'}') {
                return Ok(doc);
            }
            let key = match self.s.get(self.i) {
                Some(b'"' | b'\'') => self.string()?,
                _ => match self.word() {
                    Some(key) => key,
                    None => bail!("expected a key at {}", self.near()),
                },
            };
            self.expect(b':')?;
            let value = self.value()?;
            doc.insert(key, value);
            self.blank();
            if !self.eat(b',') {
                self.expect(b'}')?;
                return Ok(doc);
            }
        }
    }

    fn array(&mut self) -> Result<Bson> {
        self.i += 1;
        let mut items = Vec::new();
        loop {
            self.blank();
            if self.eat(b']') {
                return Ok(Bson::Array(items));
            }
            items.push(self.value()?);
            self.blank();
            if !self.eat(b',') {
                self.expect(b']')?;
                return Ok(Bson::Array(items));
            }
        }
    }

    /// A string: to its closing quote (a `\\` takes the character after it
    /// along), decoded by the one decoder the checks use too.
    fn string(&mut self) -> Result<String> {
        let quote = self.s[self.i];
        let start = self.i + 1;
        let mut i = start;
        loop {
            match self.s.get(i) {
                None => bail!("a string isn't closed"),
                Some(b'\\') => i += 2,
                Some(&c) if c == quote => break,
                Some(_) => i += 1,
            }
        }
        let body = self.text.get(start..i).ok_or_else(|| anyhow!("a string isn't closed"))?;
        self.i = i + 1;
        super::language::unescape(body).map_err(|e| anyhow!(e))
    }

    fn number(&mut self) -> Result<Bson> {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || b".-+".contains(&self.s[self.i])) {
            self.i += 1;
        }
        let n = &self.text[start..self.i];
        if let Ok(i) = n.parse::<i64>() {
            return Ok(i32::try_from(i).map_or(Bson::Int64(i), Bson::Int32));
        }
        n.parse::<f64>().map(Bson::Double).map_err(|_| anyhow!("`{n}` isn't a number"))
    }

    /// A plain word: a key or a name (`$gt`, `_id`, `a.b`).
    fn word(&mut self) -> Option<String> {
        let start = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || b"_$.".contains(&self.s[self.i]) || !self.s[self.i].is_ascii())
        {
            self.i += 1;
        }
        (self.i > start).then(|| self.text[start..self.i].to_string())
    }

    /// `true`, `false`, `null`, or one of the shell's helpers.
    fn word_value(&mut self) -> Result<Bson> {
        let Some(word) = self.word() else { bail!("unexpected {}", self.near()) };
        let word = if word == "new" {
            self.blank();
            let Some(ty) = self.word() else { bail!("expected a type after `new` at {}", self.near()) };
            ty
        } else {
            word
        };
        Ok(match word.as_str() {
            "true" => Bson::Boolean(true),
            "false" => Bson::Boolean(false),
            "null" | "undefined" => Bson::Null,
            helper => {
                let arg = self.helper_argument()?;
                let text = || match &arg {
                    Some(Bson::String(s)) => Ok(s.clone()),
                    Some(other) => Ok(other.to_string()),
                    None => bail!("{helper}() needs a value"),
                };
                match helper {
                    "ObjectId" => match arg {
                        None => Bson::ObjectId(::mongodb::bson::oid::ObjectId::new()),
                        Some(_) => Bson::ObjectId(::mongodb::bson::oid::ObjectId::parse_str(text()?)?),
                    },
                    "ISODate" | "Date" => match arg {
                        None => Bson::DateTime(::mongodb::bson::DateTime::now()),
                        Some(Bson::Int32(ms)) => Bson::DateTime(::mongodb::bson::DateTime::from_millis(ms.into())),
                        Some(Bson::Int64(ms)) => Bson::DateTime(::mongodb::bson::DateTime::from_millis(ms)),
                        Some(_) => Bson::DateTime(shell_date(&text()?)?),
                    },
                    "NumberLong" => Bson::Int64(text()?.trim().parse()?),
                    "NumberInt" => Bson::Int32(text()?.trim().parse()?),
                    "NumberDecimal" => Bson::Decimal128(text()?.trim().parse()?),
                    other => bail!("`{other}` isn't a value Podėlis reads (a document, array, string, number, true, false, null, ObjectId, ISODate, NumberLong, NumberInt, NumberDecimal)"),
                }
            }
        })
    }

    /// The one value in a helper's parentheses, if any.
    fn helper_argument(&mut self) -> Result<Option<Bson>> {
        self.expect(b'(')?;
        self.blank();
        if self.eat(b')') {
            return Ok(None);
        }
        let value = self.value()?;
        self.expect(b')')?;
        Ok(Some(value))
    }
}

/// A date as the shell's `ISODate()` reads it: the day alone is that day's
/// midnight, a time without a zone is UTC, and its seconds may be left out.
fn shell_date(text: &str) -> anyhow::Result<::mongodb::bson::DateTime> {
    let text = text.trim();
    let full = match text.split_once(['T', 't', ' ']) {
        None => format!("{text}T00:00:00Z"),
        Some((day, time)) => {
            let zone_at = time.find(['Z', 'z', '+', '-']).unwrap_or(time.len());
            let (clock, zone) = time.split_at(zone_at);
            let seconds = if clock.matches(':').count() == 1 { ":00" } else { "" };
            let zone = if zone.is_empty() { "Z" } else { zone };
            format!("{day}T{clock}{seconds}{zone}")
        }
    };
    ::mongodb::bson::DateTime::parse_rfc3339_str(&full)
        .map_err(|_| anyhow::anyhow!("`{text}` isn't a date: write it like 2026-01-05 or 2026-01-05T10:00:00Z"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::mongodb::bson::doc;

    #[test]
    fn relaxed_json_as_mongosh_takes_it() {
        let got = values("{age: {$gt: 30}, 'name': \"Ada\", tags: ['a', 'b',],}, {name: 1, _id: 0}").unwrap();
        assert_eq!(got[0], Bson::Document(doc! {"age": {"$gt": 30}, "name": "Ada", "tags": ["a", "b"]}));
        assert_eq!(got[1], Bson::Document(doc! {"name": 1, "_id": 0}));
        assert_eq!(values("").unwrap(), Vec::<Bson>::new());
        assert_eq!(values("5, -2.5, true, null").unwrap(), [Bson::Int32(5), Bson::Double(-2.5), Bson::Boolean(true), Bson::Null]);
        assert_eq!(values("NumberLong('9000000000')").unwrap(), [Bson::Int64(9_000_000_000)]);
    }

    #[test]
    fn the_shell_s_helpers() {
        let id = "64b7f0c2a1b2c3d4e5f60718";
        let got = values(&format!("ObjectId(\"{id}\"), ISODate('2026-11-03T10:00:00Z'), new Date(0)")).unwrap();
        assert_eq!(got[0], Bson::ObjectId(::mongodb::bson::oid::ObjectId::parse_str(id).unwrap()));
        assert!(matches!(got[1], Bson::DateTime(_)));
        assert_eq!(got[2], Bson::DateTime(::mongodb::bson::DateTime::from_millis(0)));
    }

    #[test]
    fn a_date_reads_as_the_shell_reads_it() {
        let millis = |text: &str| shell_date(text).unwrap().timestamp_millis();
        let midnight = millis("2026-01-05T00:00:00Z");
        assert_eq!(millis("2026-01-05"), midnight);
        assert_eq!(millis("2026-01-05T00:00"), midnight);
        assert_eq!(millis("2026-01-05 00:00:00"), midnight);
        assert_eq!(millis("2026-01-05T02:00:00+02:00"), midnight);
        assert_eq!(millis("2026-01-05T01:00+01:00"), midnight);
        assert_eq!(millis("2026-01-05T00:00:00.000Z"), midnight);
        // In a statement, and by `new Date`.
        let got = values("ISODate('2026-01-05'), new Date('2026-01-05')").unwrap();
        assert_eq!(got[0], Bson::DateTime(::mongodb::bson::DateTime::from_millis(midnight)));
        assert_eq!(got[1], got[0]);
        // What isn't one says what it wants, not what the parser tripped on.
        for bad in ["yesterday", "2026-13-05", "2026-01-05T25:00"] {
            let said = shell_date(bad).unwrap_err().to_string();
            assert!(said.contains(bad) && said.contains("2026-01-05T10:00:00Z"), "{said}");
        }
    }

    #[test]
    fn what_it_can_t_read_says_where() {
        assert!(values("{a: }").unwrap_err().to_string().contains("unexpected"));
        assert!(values("{a: 1").unwrap_err().to_string().contains("expected `}`"));
        assert!(values("Code('x')").unwrap_err().to_string().contains("isn't a value"));
        assert!(document(Some(&Bson::Int32(1)), "the filter").is_err());
        // A backslash before a character of more than a byte.
        assert_eq!(values(r#""\ą\$x""#).unwrap(), [Bson::String("ą$x".into())]);
    }
}
