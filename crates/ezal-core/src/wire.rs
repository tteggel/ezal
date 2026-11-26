//! Private, bounded planning snapshot codec. Fixed-width little-endian integers
//! and IEEE float bits; no Rust memory layout, pointers, padding or allocations.

pub(crate) struct Writer<'a> {
    pub data: &'a mut [u8],
    pub at: usize,
}
pub(crate) struct Reader<'a>(pub &'a [u8]);
pub(crate) trait Wire: Sized {
    fn encode(&self, w: &mut Writer<'_>) -> Option<()>;
    fn decode(r: &mut Reader<'_>) -> Option<Self>;
}
macro_rules! wire_struct {
    ($t:ty { $($field:ident),* }) => {
        impl crate::wire::Wire for $t {
            fn encode(&self, w: &mut crate::wire::Writer<'_>) -> Option<()> {
                $(crate::wire::Wire::encode(&self.$field, w)?;)* Some(())
            }
            fn decode(r: &mut crate::wire::Reader<'_>) -> Option<Self> {
                Some(Self { $($field: crate::wire::Wire::decode(r)?),* })
            }
        }
    };
}
macro_rules! wire_enum {
    ($t:ty { $($n:literal => $v:ident),* }) => {
        impl crate::wire::Wire for $t {
            fn encode(&self, w: &mut crate::wire::Writer<'_>) -> Option<()> {
                let tag: u8 = match self { $(Self::$v => $n),* };
                crate::wire::Wire::encode(&tag, w)
            }
            fn decode(r: &mut crate::wire::Reader<'_>) -> Option<Self> {
                match <u8 as crate::wire::Wire>::decode(r)? { $($n => Some(Self::$v),)* _ => None }
            }
        }
    };
}
macro_rules! number {
    ($($t:ty),*) => { $(impl Wire for $t {
        fn encode(&self, w: &mut Writer<'_>) -> Option<()> {
            let bytes = self.to_le_bytes();
            let end = w.at.checked_add(bytes.len())?;
            w.data.get_mut(w.at..end)?.copy_from_slice(&bytes);
            w.at = end;
            Some(())
        }
        fn decode(r: &mut Reader<'_>) -> Option<Self> {
            let n = core::mem::size_of::<Self>();
            let value = Self::from_le_bytes(r.0.get(..n)?.try_into().ok()?);
            r.0 = &r.0[n..];
            Some(value)
        }
    })* };
}
number!(u8, i8, u16, u32, i32, u64, f32, f64);
impl Wire for bool {
    fn encode(&self, w: &mut Writer<'_>) -> Option<()> {
        (*self as u8).encode(w)
    }
    fn decode(r: &mut Reader<'_>) -> Option<Self> {
        match u8::decode(r)? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}
impl<T: Wire> Wire for Option<T> {
    fn encode(&self, w: &mut Writer<'_>) -> Option<()> {
        self.is_some().encode(w)?;
        if let Some(value) = self {
            value.encode(w)?;
        }
        Some(())
    }
    fn decode(r: &mut Reader<'_>) -> Option<Self> {
        Some(if bool::decode(r)? {
            Some(T::decode(r)?)
        } else {
            None
        })
    }
}
impl<T: Wire, const N: usize> Wire for [T; N] {
    fn encode(&self, w: &mut Writer<'_>) -> Option<()> {
        for value in self {
            value.encode(w)?;
        }
        Some(())
    }
    fn decode(r: &mut Reader<'_>) -> Option<Self> {
        let values: [Option<T>; N] = core::array::from_fn(|_| T::decode(r));
        if values.iter().any(Option::is_none) {
            return None;
        }
        Some(values.map(|value| value.expect("checked array")))
    }
}
impl<T: Wire, const N: usize> Wire for heapless::Vec<T, N> {
    fn encode(&self, w: &mut Writer<'_>) -> Option<()> {
        u32::try_from(self.len()).ok()?.encode(w)?;
        for value in self {
            value.encode(w)?;
        }
        Some(())
    }
    fn decode(r: &mut Reader<'_>) -> Option<Self> {
        let len = u32::decode(r)? as usize;
        if len > N {
            return None;
        }
        let mut out = Self::new();
        for _ in 0..len {
            out.push(T::decode(r)?).ok()?;
        }
        Some(out)
    }
}
