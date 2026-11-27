//! Save states.
//!
//! Every component implements one `snap(&mut self, s: &mut Snap)` method that
//! is used for both saving and loading, so the field order can't diverge
//! between the two directions. Disc contents and memory cards are not part
//! of a state (the disc is re-attached; cards live in their own files).

pub const MAGIC: &[u8; 8] = b"PSXSTATE";
pub const VERSION: u32 = 4;

enum Mode<'a> {
    Save(Vec<u8>),
    Load(&'a [u8], usize),
}

pub struct Snap<'a> {
    mode: Mode<'a>,
    pub error: Option<String>,
}

impl<'a> Snap<'a> {
    pub fn saver() -> Snap<'static> {
        Snap { mode: Mode::Save(Vec::with_capacity(4 << 20)), error: None }
    }

    pub fn loader(data: &'a [u8]) -> Snap<'a> {
        Snap { mode: Mode::Load(data, 0), error: None }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.mode, Mode::Load(..))
    }

    pub fn into_bytes(self) -> Vec<u8> {
        match self.mode {
            Mode::Save(v) => v,
            Mode::Load(..) => vec![],
        }
    }

    pub fn finished_cleanly(&self) -> bool {
        match &self.mode {
            Mode::Load(d, p) => self.error.is_none() && *p == d.len(),
            Mode::Save(_) => self.error.is_none(),
        }
    }

    fn bytes(&mut self, buf: &mut [u8]) {
        match &mut self.mode {
            Mode::Save(v) => v.extend_from_slice(buf),
            Mode::Load(d, p) => {
                if self.error.is_some() {
                    return;
                }
                if *p + buf.len() > d.len() {
                    self.error = Some("save state is truncated".into());
                    return;
                }
                buf.copy_from_slice(&d[*p..*p + buf.len()]);
                *p += buf.len();
            }
        }
    }

    pub fn u8(&mut self, v: &mut u8) {
        let mut b = [*v];
        self.bytes(&mut b);
        *v = b[0];
    }
    pub fn bool(&mut self, v: &mut bool) {
        let mut b = *v as u8;
        self.u8(&mut b);
        *v = b != 0;
    }
    pub fn u16(&mut self, v: &mut u16) {
        let mut b = v.to_le_bytes();
        self.bytes(&mut b);
        *v = u16::from_le_bytes(b);
    }
    pub fn i16(&mut self, v: &mut i16) {
        let mut u = *v as u16;
        self.u16(&mut u);
        *v = u as i16;
    }
    pub fn u32(&mut self, v: &mut u32) {
        let mut b = v.to_le_bytes();
        self.bytes(&mut b);
        *v = u32::from_le_bytes(b);
    }
    pub fn i32(&mut self, v: &mut i32) {
        let mut u = *v as u32;
        self.u32(&mut u);
        *v = u as i32;
    }
    pub fn u64(&mut self, v: &mut u64) {
        let mut b = v.to_le_bytes();
        self.bytes(&mut b);
        *v = u64::from_le_bytes(b);
    }
    pub fn i64(&mut self, v: &mut i64) {
        let mut u = *v as u64;
        self.u64(&mut u);
        *v = u as i64;
    }
    pub fn usize(&mut self, v: &mut usize) {
        let mut u = *v as u64;
        self.u64(&mut u);
        *v = u as usize;
    }

    /// Fixed-size byte buffer (length must match on load).
    pub fn buf(&mut self, v: &mut [u8]) {
        let mut n = v.len() as u32;
        self.u32(&mut n);
        if self.is_loading() && n as usize != v.len() {
            self.error.get_or_insert(format!("buffer size mismatch: {} vs {}", n, v.len()));
            return;
        }
        self.bytes(v);
    }

    /// Variable-length byte vector.
    pub fn vec_u8(&mut self, v: &mut Vec<u8>) {
        let mut n = v.len() as u32;
        self.u32(&mut n);
        if self.is_loading() {
            if n > 64 << 20 {
                self.error.get_or_insert("implausible vector length".into());
                return;
            }
            v.resize(n as usize, 0);
        }
        self.bytes(v);
    }

    pub fn vec_u16(&mut self, v: &mut Vec<u16>) {
        let mut n = v.len() as u32;
        self.u32(&mut n);
        if self.is_loading() {
            if n > 32 << 20 {
                self.error.get_or_insert("implausible vector length".into());
                return;
            }
            v.resize(n as usize, 0);
        }
        for x in v.iter_mut() {
            self.u16(x);
        }
    }

    pub fn vec_u32(&mut self, v: &mut Vec<u32>) {
        let mut n = v.len() as u32;
        self.u32(&mut n);
        if self.is_loading() {
            if n > 16 << 20 {
                self.error.get_or_insert("implausible vector length".into());
                return;
            }
            v.resize(n as usize, 0);
        }
        for x in v.iter_mut() {
            self.u32(x);
        }
    }

    pub fn arr_u32(&mut self, v: &mut [u32]) {
        for x in v.iter_mut() {
            self.u32(x);
        }
    }

    pub fn arr_i32(&mut self, v: &mut [i32]) {
        for x in v.iter_mut() {
            self.i32(x);
        }
    }

    pub fn arr_i16(&mut self, v: &mut [i16]) {
        for x in v.iter_mut() {
            self.i16(x);
        }
    }

    pub fn arr_u16(&mut self, v: &mut [u16]) {
        for x in v.iter_mut() {
            self.u16(x);
        }
    }

    /// Section marker: catches field-order mistakes early on load.
    pub fn tag(&mut self, name: &str) {
        let mut t = [0u8; 4];
        let src = name.as_bytes();
        t[..src.len().min(4)].copy_from_slice(&src[..src.len().min(4)]);
        let want = t;
        self.bytes(&mut t);
        if self.is_loading() && t != want && self.error.is_none() {
            self.error = Some(format!("corrupt save state at section '{}'", name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_primitives() {
        let mut s = Snap::saver();
        let (mut a, mut b, mut c, mut d) = (0xABu8, true, -1234i32, vec![1u8, 2, 3]);
        let mut e = vec![0xBEEFu16; 3];
        s.tag("TEST");
        s.u8(&mut a);
        s.bool(&mut b);
        s.i32(&mut c);
        s.vec_u8(&mut d);
        s.vec_u16(&mut e);
        let bytes = s.into_bytes();

        let mut l = Snap::loader(&bytes);
        let (mut a2, mut b2, mut c2, mut d2, mut e2) = (0u8, false, 0i32, vec![], vec![]);
        l.tag("TEST");
        l.u8(&mut a2);
        l.bool(&mut b2);
        l.i32(&mut c2);
        l.vec_u8(&mut d2);
        l.vec_u16(&mut e2);
        assert!(l.finished_cleanly());
        assert_eq!((a2, b2, c2, d2, e2), (a, b, c, d, e));
    }

    #[test]
    fn truncated_and_corrupt_inputs_are_errors() {
        let mut s = Snap::saver();
        let mut x = 5u32;
        s.tag("AAAA");
        s.u32(&mut x);
        let bytes = s.into_bytes();
        let mut l = Snap::loader(&bytes[..6]);
        l.tag("AAAA");
        l.u32(&mut x);
        assert!(l.error.is_some());
        let mut l = Snap::loader(&bytes);
        l.tag("BBBB");
        assert!(l.error.is_some());
    }
}
