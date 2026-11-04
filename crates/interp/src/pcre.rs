//! A direct binding to PCRE2 for `perl = TRUE` patterns.
//!
//! The `pcre2` crate sets `PCRE2_MATCH_INVALID_UTF` only together with `PCRE2_UCP`, which GNU R
//! does not use, so in plain UTF mode every `pcre2_match` call validated the whole subject. A
//! global `gsub` calls it once per match, which made it quadratic: 200,000 replacements in a
//! 1 MB string took two minutes. Subjects here are Rust strings and so always valid UTF-8, which
//! lets every call pass `PCRE2_NO_UTF_CHECK`, as GNU R does after validating its input once.

use {
    libc::c_void,
    pcre2_sys::{
        PCRE2_CASELESS, PCRE2_ERROR_NOMATCH, PCRE2_INFO_CAPTURECOUNT, PCRE2_INFO_NAMECOUNT,
        PCRE2_INFO_NAMEENTRYSIZE, PCRE2_INFO_NAMETABLE, PCRE2_JIT_COMPLETE, PCRE2_NO_UTF_CHECK,
        PCRE2_UTF, pcre2_code_8, pcre2_code_free_8, pcre2_compile_8, pcre2_get_error_message_8,
        pcre2_get_ovector_pointer_8, pcre2_jit_compile_8, pcre2_jit_stack_8,
        pcre2_jit_stack_assign_8, pcre2_jit_stack_create_8, pcre2_jit_stack_free_8, pcre2_match_8,
        pcre2_match_context_8, pcre2_match_context_create_8, pcre2_match_context_free_8,
        pcre2_match_data_8, pcre2_match_data_create_from_pattern_8, pcre2_match_data_free_8,
        pcre2_pattern_info_8,
    },
    std::ptr::{self, NonNull},
};

/// A compiled pattern with the match state every search through it reuses. Matching never
/// re-enters itself, so one match data block serves every search.
pub(crate) struct PerlRegex {
    code: NonNull<pcre2_code_8>,
    context: NonNull<pcre2_match_context_8>,
    data: NonNull<pcre2_match_data_8>,
    stack: Option<NonNull<pcre2_jit_stack_8>>,
    /// The number of groups, counting the whole match as group 0.
    groups: usize,
    names: Vec<Option<String>>,
}

/// Why a pattern failed to compile: PCRE2's message and the byte offset it points at.
pub(crate) struct CompileError {
    pub(crate) message: String,
    pub(crate) offset: usize,
}

/// GNU R's default `R_PCRE_JIT_STACK_MAXSIZE`, 64 MB; PCRE2's own 32 KB fails on a long subject
/// such as 5,000 repeats of `(a|b)+`.
const JIT_STACK_MAX: usize = 64 * 1024 * 1024;
const JIT_STACK_START: usize = 32 * 1024;

impl PerlRegex {
    /// Compiles `pattern` in UTF mode, caseless when asked, and JIT-compiled when PCRE2 supports
    /// it, as GNU R's `R_pcre2_prepare` does.
    pub(crate) fn new(pattern: &str, caseless: bool) -> Result<PerlRegex, CompileError> {
        let options = PCRE2_UTF | if caseless { PCRE2_CASELESS } else { 0 };
        let mut error_code = 0;
        let mut offset = 0;
        // SAFETY: the pattern pointer and length describe a live string, and a null compile
        // context selects PCRE2's defaults.
        let code = unsafe {
            pcre2_compile_8(
                pattern.as_ptr(),
                pattern.len(),
                options,
                &mut error_code,
                &mut offset,
                ptr::null_mut(),
            )
        };
        let Some(code) = NonNull::new(code) else {
            return Err(CompileError {
                message: error_message(error_code),
                offset,
            });
        };
        // SAFETY: `code` is a live compiled pattern; each allocation is checked and freed by
        // `Drop` or below when a later one fails.
        unsafe {
            let jit = pcre2_jit_compile_8(code.as_ptr(), PCRE2_JIT_COMPLETE) == 0;
            let context = NonNull::new(pcre2_match_context_create_8(ptr::null_mut()));
            let data = NonNull::new(pcre2_match_data_create_from_pattern_8(
                code.as_ptr(),
                ptr::null_mut(),
            ));
            let (Some(context), Some(data)) = (context, data) else {
                if let Some(context) = context {
                    pcre2_match_context_free_8(context.as_ptr());
                }
                if let Some(data) = data {
                    pcre2_match_data_free_8(data.as_ptr());
                }
                pcre2_code_free_8(code.as_ptr());
                return Err(CompileError {
                    message: "out of memory".to_owned(),
                    offset: 0,
                });
            };
            let stack = if jit {
                NonNull::new(pcre2_jit_stack_create_8(
                    JIT_STACK_START,
                    JIT_STACK_MAX,
                    ptr::null_mut(),
                ))
            } else {
                None
            };
            if let Some(stack) = stack {
                pcre2_jit_stack_assign_8(context.as_ptr(), None, stack.as_ptr() as *mut c_void);
            }
            let mut regex = PerlRegex {
                code,
                context,
                data,
                stack,
                groups: 1,
                names: Vec::new(),
            };
            regex.groups = regex.info(PCRE2_INFO_CAPTURECOUNT) + 1;
            regex.names = regex.read_names();
            Ok(regex)
        }
    }

    /// The number of groups, counting the whole match as group 0.
    pub(crate) fn groups(&self) -> usize {
        self.groups
    }

    /// Each group's name, `None` for group 0 and for an unnamed group.
    pub(crate) fn names(&self) -> &[Option<String>] {
        &self.names
    }

    /// The first match at or after byte `from`, as a byte range.
    pub(crate) fn find_at(
        &self,
        subject: &str,
        from: usize,
    ) -> Result<Option<(usize, usize)>, String> {
        let Some(ovector) = self.search(subject, from)? else {
            return Ok(None);
        };
        Ok(ovector.into_iter().next().flatten())
    }

    /// The first match at or after byte `from`, with every group's byte range.
    pub(crate) fn captures_at(
        &self,
        subject: &str,
        from: usize,
    ) -> Result<Option<Vec<Option<(usize, usize)>>>, String> {
        Ok(self.search(subject, from)?.map(|groups| groups.collect()))
    }

    /// Runs one search and returns the group ranges it set, read lazily from the match data.
    fn search(
        &self,
        subject: &str,
        from: usize,
    ) -> Result<Option<impl Iterator<Item = Option<(usize, usize)>> + '_>, String> {
        // SAFETY: the subject is valid UTF-8 because it is a `str`, which is what
        // `PCRE2_NO_UTF_CHECK` requires; `from` is at most its length.
        let result = unsafe {
            pcre2_match_8(
                self.code.as_ptr(),
                subject.as_ptr(),
                subject.len(),
                from.min(subject.len()),
                PCRE2_NO_UTF_CHECK,
                self.data.as_ptr(),
                self.context.as_ptr(),
            )
        };
        if result == PCRE2_ERROR_NOMATCH {
            return Ok(None);
        }
        if result < 0 {
            return Err(error_message(result));
        }
        // SAFETY: the match data was created from this pattern, so its ovector holds a start
        // and an end for every group.
        let ovector = unsafe {
            std::slice::from_raw_parts(
                pcre2_get_ovector_pointer_8(self.data.as_ptr()),
                self.groups * 2,
            )
        };
        Ok(Some(ovector.chunks_exact(2).map(|pair| match *pair {
            [start, end] if start != usize::MAX => Some((start, end)),
            _ => None,
        })))
    }

    unsafe fn info(&self, what: u32) -> usize {
        let mut value: u32 = 0;
        // SAFETY: every query this module makes returns a `uint32_t`.
        let status = unsafe {
            pcre2_pattern_info_8(
                self.code.as_ptr(),
                what,
                &mut value as *mut u32 as *mut c_void,
            )
        };
        if status == 0 { value as usize } else { 0 }
    }

    /// Reads the name table: fixed-size entries of a big-endian group number followed by the
    /// NUL-terminated name.
    unsafe fn read_names(&self) -> Vec<Option<String>> {
        let mut names = vec![None; self.groups];
        // SAFETY: the counts come from the same pattern as the table they describe.
        unsafe {
            let count = self.info(PCRE2_INFO_NAMECOUNT);
            let size = self.info(PCRE2_INFO_NAMEENTRYSIZE);
            let mut table: *const u8 = ptr::null();
            let status = pcre2_pattern_info_8(
                self.code.as_ptr(),
                PCRE2_INFO_NAMETABLE,
                &mut table as *mut *const u8 as *mut c_void,
            );
            if status != 0 || table.is_null() || size < 3 {
                return names;
            }
            let table = std::slice::from_raw_parts(table, count * size);
            for entry in table.chunks_exact(size) {
                let group = (entry[0] as usize) << 8 | entry[1] as usize;
                let name = &entry[2..];
                let name = &name[..name
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(name.len())];
                if let Some(slot) = names.get_mut(group) {
                    *slot = Some(String::from_utf8_lossy(name).into_owned());
                }
            }
        }
        names
    }
}

impl Drop for PerlRegex {
    fn drop(&mut self) {
        // SAFETY: each pointer was allocated by `new` and is freed exactly once here.
        unsafe {
            pcre2_match_data_free_8(self.data.as_ptr());
            pcre2_match_context_free_8(self.context.as_ptr());
            if let Some(stack) = self.stack {
                pcre2_jit_stack_free_8(stack.as_ptr());
            }
            pcre2_code_free_8(self.code.as_ptr());
        }
    }
}

fn error_message(code: i32) -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer and its length describe writable memory.
    let length = unsafe { pcre2_get_error_message_8(code, buffer.as_mut_ptr(), buffer.len()) };
    match usize::try_from(length) {
        Ok(length) => String::from_utf8_lossy(&buffer[..length.min(buffer.len())]).into_owned(),
        Err(_) => format!("PCRE2 error {code}"),
    }
}
