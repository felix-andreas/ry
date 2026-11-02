//! The `Content-Length` framing that `ry debug-adapter` reads from its client and the language
//! server's console link reads from a console (`ry::debug_adapter::read_message`,
//! `ry::console_link::read_message`). Generated streams of valid frames, with odd but legal
//! headers, must read back exactly what the writer framed, and must stop exactly at each declared
//! length. Mutated and random streams must never panic or loop, and a counting allocator holds
//! every read to memory proportional to the input it was given, however large a length a header
//! claims. Each seed is replayable with the `FUZZ_SEED` command a failure prints, and
//! `FUZZ_ITERS` sets how many seeds run.

use {
    ry::{
        console_link,
        debug_adapter::{MAX_HEADER_LINE, MAX_MESSAGE, read_message, write_message},
    },
    serde_json::{Value, json},
    std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
        io::{BufReader, Cursor, Read},
    },
    syntax::testing::{SplitMix64, for_each_seed},
};

/// Records the largest allocation the current thread makes while [`largest_allocation`] runs.
struct Counting;

thread_local! {
    /// `None` while nothing is measured on this thread.
    static LARGEST: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record(size: usize) {
    // The thread's local storage may already be gone while the thread exits; nothing is
    // measured then.
    if let Ok(Some(largest)) = LARGEST.try_with(Cell::get) {
        LARGEST.with(|cell| cell.set(Some(largest.max(size))));
    }
}

// SAFETY: every call is forwarded to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `body` and returns its value with the largest single allocation it made.
fn largest_allocation<T>(body: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|cell| cell.set(Some(0)));
    let value = body();
    let largest = LARGEST.with(|cell| cell.replace(None)).unwrap_or(0);
    (value, largest)
}

/// What a read may allocate beyond the input: the reader's line and the growth of a body
/// buffer, which at most double what arrived.
fn allowed_allocation(input: usize) -> usize {
    2 * input + 4 * MAX_HEADER_LINE
}

#[test]
fn valid_frames_read_back_exactly() {
    for_each_seed(400, |seed| {
        let mut entropy = SplitMix64(seed);
        let mut stream = Vec::new();
        let mut bodies = Vec::new();
        for _ in 0..entropy.below(6) {
            let body = random_body(&mut entropy);
            match entropy.chance(1, 2) {
                true => write_message(&mut stream, &body).expect("writing to memory succeeds"),
                false => stream.extend(odd_frame(&mut entropy, &body)),
            }
            bodies.push(body);
        }
        // A header that never ends must be left unread by every message before it.
        let tail: &[u8] = match entropy.chance(1, 2) {
            true => b"Content-Len",
            false => b"",
        };
        stream.extend(tail);
        let mut reader = BufReader::with_capacity(1 + entropy.below(64), Cursor::new(&stream[..]));
        for (index, expected) in bodies.iter().enumerate() {
            let body = read_message(&mut reader)
                .unwrap_or_else(|error| panic!("message {index} failed: {error}"))
                .unwrap_or_else(|| panic!("message {index} is missing"));
            assert_eq!(&body, expected, "message {index} read back differently");
        }
        let mut rest = Vec::new();
        reader
            .read_to_end(&mut rest)
            .expect("reading memory succeeds");
        assert_eq!(rest, tail, "the reader went past the last declared length");
        let mut reader = BufReader::new(Cursor::new(tail));
        match tail.is_empty() {
            true => assert!(matches!(read_message(&mut reader), Ok(None))),
            false => assert!(read_message(&mut reader).is_err()),
        }
    });
}

#[test]
fn the_console_link_reads_back_framed_json() {
    for_each_seed(200, |seed| {
        let mut entropy = SplitMix64(seed);
        let messages: Vec<Value> = (0..1 + entropy.below(5))
            .map(|index| {
                json!({
                    "type": "response",
                    "request_seq": index,
                    "success": entropy.chance(1, 2),
                    "body": { "text": String::from_utf8_lossy(&random_body(&mut entropy)) },
                })
            })
            .collect();
        let mut stream = Vec::new();
        for message in &messages {
            let body = message.to_string().into_bytes();
            match entropy.chance(1, 2) {
                true => write_message(&mut stream, &body).expect("writing to memory succeeds"),
                false => stream.extend(odd_frame(&mut entropy, &body)),
            }
        }
        let mut reader = BufReader::new(Cursor::new(&stream[..]));
        for (index, expected) in messages.iter().enumerate() {
            let message = console_link::read_message(&mut reader)
                .unwrap_or_else(|| panic!("message {index} is missing"));
            assert_eq!(&message, expected, "message {index} read back differently");
        }
        assert_eq!(console_link::read_message(&mut reader), None);
    });
}

/// Broken framing ends a stream with an error or `None`, after at most one read per input byte,
/// and never allocates much more than the input, however large a length a header claims.
#[test]
fn mutated_frames_never_panic_loop_or_overallocate() {
    for_each_seed(600, |seed| {
        let mut entropy = SplitMix64(seed);
        let stream = mutated_stream(&mut entropy);
        let ((), largest) = largest_allocation(|| {
            let mut reader = BufReader::new(Cursor::new(&stream[..]));
            let mut reads = 0;
            while let Ok(Some(body)) = read_message(&mut reader) {
                assert!(body.len() <= stream.len(), "a body larger than the input");
                reads += 1;
                assert!(reads <= stream.len(), "the reader loops");
            }
            let mut reader = BufReader::new(Cursor::new(&stream[..]));
            let mut reads = 0;
            while console_link::read_message(&mut reader).is_some() {
                reads += 1;
                assert!(reads <= stream.len(), "the console link's reader loops");
            }
        });
        assert!(
            largest <= allowed_allocation(stream.len()) + json_overhead(stream.len()),
            "reading {} bytes allocated {largest} bytes at once",
            stream.len()
        );
    });
}

// A header that claimed a large length made the reader allocate that whole length before a
// single body byte arrived, so a short corrupt frame cost up to 64 MiB.
#[test]
fn a_huge_declared_length_allocates_only_what_arrives() {
    let stream = format!("Content-Length: {}\r\n\r\n{{}}", MAX_MESSAGE).into_bytes();
    let (outcome, largest) = largest_allocation(|| {
        let mut reader = BufReader::new(Cursor::new(&stream[..]));
        read_message(&mut reader).map(|body| body.map(|body| body.len()))
    });
    assert!(outcome.is_err(), "a truncated body must fail: {outcome:?}");
    assert!(
        largest < 64 << 10,
        "allocated {largest} bytes for a 2-byte body"
    );
}

// A header line with no line break was read whole, so input without a newline grew one string
// without bound.
#[test]
fn a_header_line_without_a_break_is_refused() {
    let stream = vec![b'a'; 1 << 20];
    let (outcome, largest) = largest_allocation(|| {
        let mut reader = BufReader::new(Cursor::new(&stream[..]));
        read_message(&mut reader).map(|body| body.map(|body| body.len()))
    });
    assert!(
        outcome.is_err(),
        "an endless header line must fail: {outcome:?}"
    );
    assert!(
        largest < 64 << 10,
        "allocated {largest} bytes for one header line"
    );
}

#[test]
fn lengths_beyond_the_limit_or_unparsable_are_refused() {
    for header in [
        format!("Content-Length: {}", MAX_MESSAGE + 1),
        format!("Content-Length: {}", u64::MAX),
        "Content-Length: 99999999999999999999999999".to_owned(),
        "Content-Length: -1".to_owned(),
        "Content-Length: 0x10".to_owned(),
        "Content-Length:".to_owned(),
        "Content-Type: application/json".to_owned(),
    ] {
        let stream = format!("{header}\r\n\r\n{{}}").into_bytes();
        let mut reader = BufReader::new(Cursor::new(&stream[..]));
        assert!(
            read_message(&mut reader).is_err(),
            "`{header}` was accepted"
        );
    }
}

/// Parsing a body as JSON allocates for its values, which a byte budget for framing does not
/// cover; a JSON value costs a small multiple of its text.
fn json_overhead(input: usize) -> usize {
    8 * input + 4096
}

/// A frame whose headers are spelled as a client may legally spell them.
fn odd_frame(entropy: &mut SplitMix64, body: &[u8]) -> Vec<u8> {
    let newline = match entropy.chance(1, 3) {
        true => "\n",
        false => "\r\n",
    };
    let name = [
        "Content-Length",
        "content-length",
        "CONTENT-LENGTH",
        "Content-length",
    ][entropy.below(4)];
    let separator = [": ", ":", ":   ", ": \t"][entropy.below(4)];
    let zeros = "0".repeat(entropy.below(3));
    let mut headers = vec![format!(
        "{name}{separator}{zeros}{}{}",
        body.len(),
        [" ", "\t", ""][entropy.below(3)]
    )];
    for _ in 0..entropy.below(3) {
        let other = [
            "Content-Type: application/vscode-jsonrpc; charset=utf-8",
            "X-Anything: 12: 34",
            "no colon at all",
        ][entropy.below(3)];
        headers.insert(entropy.below(headers.len() + 1), other.to_owned());
    }
    let mut frame = Vec::new();
    // Blank lines may separate one message from the next.
    for _ in 0..entropy.below(3) {
        frame.extend(newline.bytes());
    }
    for header in headers {
        frame.extend(header.bytes());
        frame.extend(newline.bytes());
    }
    frame.extend(newline.bytes());
    frame.extend(body);
    frame
}

/// Bytes a body may hold, line breaks and header-like text included, since a body is read by
/// its length alone.
fn random_body(entropy: &mut SplitMix64) -> Vec<u8> {
    let pieces: &[&[u8]] = &[
        b"{}",
        b"\r\n",
        b"\r\n\r\n",
        b"Content-Length: 3\r\n\r\n",
        b"\"text\"",
        b"\xff\xfe",
        b"\0",
        "\u{00e9}".as_bytes(),
    ];
    let mut body = Vec::new();
    for _ in 0..entropy.below(8) {
        match entropy.chance(1, 2) {
            true => body.extend(pieces[entropy.below(pieces.len())]),
            false => body.push(entropy.next_u64() as u8),
        }
    }
    body
}

/// Valid frames broken in the ways a peer breaks them: truncated, with a wrong, huge or
/// unparsable length, with garbage spliced in, or bytes flipped anywhere.
fn mutated_stream(entropy: &mut SplitMix64) -> Vec<u8> {
    let mut stream = Vec::new();
    for _ in 0..1 + entropy.below(4) {
        let body = random_body(entropy);
        match entropy.below(8) {
            0 => {
                let length = [
                    entropy.next_u64().to_string(),
                    (MAX_MESSAGE - 1).to_string(),
                    MAX_MESSAGE.to_string(),
                    (body.len() + 1 + entropy.below(100)).to_string(),
                    "-3".to_owned(),
                    "1e3".to_owned(),
                    String::new(),
                ][entropy.below(7)]
                .clone();
                stream.extend(format!("Content-Length: {length}\r\n\r\n").bytes());
                stream.extend(&body);
            }
            1 => {
                let mut frame = Vec::new();
                write_message(&mut frame, &body).expect("writing to memory succeeds");
                frame.truncate(entropy.below(frame.len()));
                stream.extend(frame);
            }
            2 => {
                for _ in 0..entropy.below(80) {
                    stream.push(entropy.next_u64() as u8);
                }
            }
            3 => stream.extend(vec![b'x'; MAX_HEADER_LINE - 1 + entropy.below(3)]),
            _ => stream.extend(odd_frame(entropy, &body)),
        }
    }
    for _ in 0..entropy.below(4) {
        if stream.is_empty() {
            break;
        }
        let at = entropy.below(stream.len());
        stream[at] = entropy.next_u64() as u8;
    }
    stream
}
