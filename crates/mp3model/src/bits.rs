//! MSB-first bit reader.

#[derive(Debug, Clone)]
pub struct Bits<'a> {
    data: &'a [u8],
    /// Position in bits from the start of `data`.
    pub pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Bits { data, pos }
    }

    /// One bit; reads past the end give 0.
    pub fn bit(&mut self) -> u32 {
        let byte = self.data.get(self.pos / 8).copied().unwrap_or(0);
        self.pos += 1;
        ((byte >> (7 - (self.pos - 1) % 8)) & 1) as u32
    }

    /// `n` bits (n <= 32), MSB first.
    pub fn read(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }

    pub fn len_bits(&self) -> usize {
        self.data.len() * 8
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_msb_first() {
        let mut b = super::Bits::new(&[0b1010_0000, 0xff]);
        assert_eq!(b.read(3), 0b101);
        assert_eq!(b.read(9), 0b0_0000_1111);
        assert_eq!(b.pos, 12);
    }
}
