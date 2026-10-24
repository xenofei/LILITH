//! Model families from file headers (CORE-164; Appendix G "Model inventory report"): the
//! safetensors `__metadata__` keys `modelspec.architecture` and `ss_base_model_version`, and
//! GGUF's `general.architecture`, within fixed limits. Pickle formats are never opened.

/// A model family read from a header, or none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Family {
    Known(String),
    Unknown,
}

/// A header that couldn't be read.
#[derive(Debug)]
pub enum HeaderError {
    Io(std::io::Error),
    Malformed(&'static str),
}

/// The family in a safetensors header; a header over 100 MiB is [`Family::Unknown`].
///
/// # Errors
/// [`HeaderError`] when the header can't be read or isn't a safetensors header.
pub fn safetensors_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError> {
    let _ = r;
    todo!("plan 4a Task 4")
}

/// The family in a GGUF header; more than 1,024 keys before `general.architecture`, or a string
/// over 64 KiB, is [`Family::Unknown`].
///
/// # Errors
/// [`HeaderError`] when the header can't be read or isn't a GGUF header.
pub fn gguf_family(r: &mut dyn std::io::Read) -> Result<Family, HeaderError> {
    let _ = r;
    todo!("plan 4a Task 4")
}
