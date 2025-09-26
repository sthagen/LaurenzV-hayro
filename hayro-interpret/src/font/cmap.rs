//! Ported from <https://github.com/mozilla/pdf.js/blob/master/src/core/cmap.js>

use std::collections::HashMap;
// Binary cmap support

const MAX_MAP_RANGE: u32 = (1 << 24) - 1; // 0xFFFFFF
const MAX_NUM_SIZE: usize = 16;
const MAX_ENCODED_NUM_SIZE: usize = MAX_NUM_SIZE; // ceil(MAX_NUM_SIZE * 7 / 8)

#[derive(Debug)]
pub(crate) struct CMap {
    codespace_ranges: [Vec<u32>; 4],
    map: HashMap<u32, u32>,
    name: String,
    vertical: bool,
}

impl CMap {
    pub(crate) fn new() -> Self {
        CMap {
            codespace_ranges: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            map: HashMap::new(),
            name: String::new(),
            vertical: false,
        }
    }

    pub(crate) fn identity_h() -> Self {
        let mut cmap = CMap::new();

        cmap.name = "Identity-H".to_string();
        cmap.vertical = false;
        cmap.add_codespace_range(2, 0, 0xFFFF);
        cmap
    }

    pub(crate) fn identity_v() -> Self {
        let mut cmap = CMap::new();

        cmap.name = "Identity-V".to_string();
        cmap.vertical = true;
        cmap.add_codespace_range(2, 0, 0xFFFF);
        cmap
    }

    pub(crate) fn is_vertical(&self) -> bool {
        self.vertical
    }

    pub(crate) fn lookup_code(&self, code: u32) -> Option<u32> {
        if let Some(value) = self.map.get(&code) {
            Some(*value)
        } else if self.is_identity_cmap() {
            if code <= 0xFFFF { Some(code) } else { None }
        } else {
            None
        }
    }

    fn add_codespace_range(&mut self, n: usize, low: u32, high: u32) {
        if n > 0 && n <= 4 {
            self.codespace_ranges[n - 1].push(low);
            self.codespace_ranges[n - 1].push(high);
        }
    }

    fn map_cid_range(&mut self, low: u32, high: u32, dst_low: u32) -> Option<()> {
        if high - low > MAX_MAP_RANGE {
            return None;
        }

        let mut current_low = low;
        let mut current_dst = dst_low;
        while current_low <= high {
            self.map.insert(current_low, current_dst);
            current_low += 1;
            current_dst += 1;
        }

        Some(())
    }

    fn map_bf_range(&mut self, low: u32, high: u32, dst_low: String) -> Option<()> {
        if high - low > MAX_MAP_RANGE {
            return None;
        }

        let mut current_low = low;
        let mut current_dst = dst_low;

        while current_low <= high {
            self.map.insert(current_low, bf_string_char(&current_dst));

            let mut bytes = current_dst.into_bytes();
            if let Some(last_byte) = bytes.last_mut() {
                if *last_byte == 0xff {
                    if bytes.len() > 1 {
                        let len = bytes.len();
                        bytes[len - 2] += 1;
                        bytes[len - 1] = 0x00;
                    }
                } else {
                    *last_byte += 1;
                }
            }
            current_dst = String::from_utf8_lossy(&bytes).to_string();
            current_low += 1;
        }

        Some(())
    }

    fn map_bf_range_to_array(&mut self, low: u32, high: u32, array: Vec<u32>) -> Option<()> {
        if high - low > MAX_MAP_RANGE {
            return None;
        }

        let mut current_low = low;
        let mut i = 0;

        while current_low <= high && i < array.len() {
            self.map.insert(current_low, array[i]);
            current_low += 1;
            i += 1;
        }

        Some(())
    }

    fn map_one(&mut self, src: u32, dst: u32) {
        self.map.insert(src, dst);
    }

    fn is_identity_cmap(&self) -> bool {
        (self.name == "Identity-H" || self.name == "Identity-V") && self.map.is_empty()
    }

    pub fn read_code(&self, bytes: &[u8], offset: usize) -> (u32, usize) {
        let mut c = 0u32;

        for n in 0..4.min(bytes.len() - offset) {
            if offset + n >= bytes.len() {
                break;
            }

            c = (c << 8) | bytes[offset + n] as u32;

            let codespace_range = &self.codespace_ranges[n];
            for chunk in codespace_range.chunks(2) {
                if chunk.len() == 2 {
                    let low = chunk[0];
                    let high = chunk[1];
                    if c >= low && c <= high {
                        return (c, n + 1);
                    }
                }
            }
        }

        (0, 1)
    }
}

fn bf_string_char(str: &str) -> u32 {
    str.chars().next().unwrap_or(0 as char) as u32
}

fn str_to_int(s: &str) -> u32 {
    let mut a = 0u32;
    for ch in s.chars() {
        // Since we created these strings from bytes using char::from(byte),
        // we can safely cast back to get the original byte value
        a = (a << 8) | (ch as u32 & 0xFF);
    }
    a
}

fn expect_string(obj: &Token) -> Option<String> {
    match obj {
        Token::HexString(bytes) => {
            // Convert bytes to string the same way pdf.js does: using String.fromCharCode
            // Each byte becomes a character with that character code
            let mut result = String::new();
            for &byte in bytes {
                result.push(char::from(byte));
            }
            Some(result)
        }
        Token::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn expect_int(obj: &Token) -> Option<i32> {
    match obj {
        Token::Integer(i) => Some(*i),
        _ => None,
    }
}

#[derive(Debug, Clone)]
enum Token {
    String(String),
    HexString(Vec<u8>), // Raw bytes from hex string
    Integer(i32),
    Command(String),
    Name(String),
    Eof,
}

struct CMapLexer<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> CMapLexer<'a> {
    fn new(input: &'a [u8]) -> Self {
        CMapLexer { input, position: 0 }
    }

    fn get_obj(&mut self) -> Token {
        self.skip_whitespace();

        if self.position >= self.input.len() {
            return Token::Eof;
        }

        let remaining = &self.input[self.position..];

        // Handle PostScript comments (% to end of line)
        if remaining.starts_with(b"%") {
            // Skip to end of line
            while self.position < self.input.len() {
                let ch = self.input[self.position];
                self.position += 1;
                if ch == b'\n' || ch == b'\r' {
                    break;
                }
            }
            // Skip any additional whitespace and try again
            self.skip_whitespace();
            return self.get_obj();
        }

        // Handle dictionary delimiters
        if remaining.starts_with(b">>") {
            self.position += 2;
            return Token::Command(">>".to_string());
        }

        // Handle hex strings and dictionary start
        if remaining.starts_with(b"<") {
            return self.parse_hex_string();
        }

        // Handle PostScript strings (parentheses)
        if remaining.starts_with(b"(") {
            return self.parse_ps_string();
        }

        // Handle arrays
        if remaining.starts_with(b"[") {
            return self.parse_array();
        }

        if remaining.starts_with(b"]") {
            self.position += 1;
            return Token::Command("]".to_string());
        }

        // Handle names
        if remaining.starts_with(b"/") {
            return self.parse_name();
        }

        // Handle numbers and commands
        self.parse_token()
    }

    fn skip_whitespace(&mut self) {
        while self.position < self.input.len() {
            let ch = self.input[self.position];
            if ch.is_ascii_whitespace() {
                self.position += 1;
            } else {
                break;
            }
        }
    }

    fn parse_hex_string(&mut self) -> Token {
        // Check if it's actually a dictionary delimiter <<
        let remaining = &self.input[self.position..];
        if remaining.starts_with(b"<<") {
            self.position += 2;
            return Token::Command("<<".to_string());
        }

        self.position += 1; // Skip '<'
        let mut hex_chars = Vec::new();

        while self.position < self.input.len() {
            let ch = self.input[self.position];
            if ch == b'>' {
                self.position += 1;
                break;
            }
            if ch.is_ascii_hexdigit() {
                hex_chars.push(ch as char);
            }
            self.position += 1;
        }

        // Convert hex string to raw bytes
        let mut result_bytes = Vec::new();
        for chunk in hex_chars.chunks(2) {
            let hex_byte = if chunk.len() == 2 {
                format!("{}{}", chunk[0], chunk[1])
            } else {
                format!("{}0", chunk[0])
            };

            if let Ok(byte_val) = u8::from_str_radix(&hex_byte, 16) {
                result_bytes.push(byte_val);
            }
        }

        Token::HexString(result_bytes)
    }

    fn parse_ps_string(&mut self) -> Token {
        self.position += 1; // Skip '('
        let mut string = String::new();
        let mut paren_depth = 1;

        while self.position < self.input.len() && paren_depth > 0 {
            let ch = self.input[self.position];
            match ch {
                b'(' => {
                    paren_depth += 1;
                    string.push(ch as char);
                }
                b')' => {
                    paren_depth -= 1;
                    if paren_depth > 0 {
                        string.push(ch as char);
                    }
                }
                b'\\' => {
                    // Handle escape sequences
                    self.position += 1;
                    if self.position < self.input.len() {
                        let escaped = self.input[self.position];
                        string.push('\\');
                        string.push(escaped as char);
                    }
                }
                _ => string.push(ch as char),
            }
            self.position += 1;
        }

        Token::String(string)
    }

    fn parse_array(&mut self) -> Token {
        self.position += 1; // Skip '['
        Token::Command("[".to_string())
    }

    fn parse_name(&mut self) -> Token {
        self.position += 1; // Skip '/'
        let mut name = String::new();

        while self.position < self.input.len() {
            let ch = self.input[self.position];
            if ch.is_ascii_whitespace() || b"[]<>(){}/%".contains(&ch) {
                break;
            }
            name.push(ch as char);
            self.position += 1;
        }

        Token::Name(name)
    }

    fn parse_token(&mut self) -> Token {
        let mut token = String::new();

        while self.position < self.input.len() {
            let ch = self.input[self.position];
            if ch.is_ascii_whitespace() || b"[]<>(){}/%".contains(&ch) {
                break;
            }
            token.push(ch as char);
            self.position += 1;
        }

        if token.is_empty() {
            return Token::Eof;
        }

        if let Ok(num) = token.parse::<i32>() {
            Token::Integer(num)
        } else {
            Token::Command(token)
        }
    }
}

fn parse_bf_char(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    loop {
        let obj = lexer.get_obj();
        match obj {
            Token::Eof => break,
            Token::Command(cmd) if cmd == "endbfchar" => return Some(()),
            ref token => {
                let src_str = expect_string(token)?;
                let src = str_to_int(&src_str);
                let dst_obj = lexer.get_obj();
                let dst_str = expect_string(&dst_obj)?;
                // For beginbfchar, if the destination is a short hex string (like <0003>),
                // it represents a Unicode code point, not a multi-byte string
                if dst_str.chars().count() <= 2 {
                    // Convert to Unicode code point
                    let code_point = str_to_int(&dst_str);
                    if let Some(unicode_char) = char::from_u32(code_point) {
                        cmap.map_one(src, unicode_char as u32);
                    } else {
                        cmap.map_one(src, bf_string_char(&dst_str));
                    }
                } else {
                    cmap.map_one(src, bf_string_char(&dst_str));
                }
            }
        }
    }

    Some(())
}

fn parse_bf_range(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    loop {
        let obj = lexer.get_obj();
        match obj {
            Token::Eof => break,
            Token::Command(cmd) if cmd == "endbfrange" => return Some(()),
            ref token => {
                let low_str = expect_string(token)?;
                let low = str_to_int(&low_str);

                let high_obj = lexer.get_obj();
                let high_str = expect_string(&high_obj)?;
                let high = str_to_int(&high_str);

                let dst_obj = lexer.get_obj();
                match dst_obj {
                    Token::Integer(dst_int) => {
                        let dst_low = char::from(dst_int as u8).to_string();
                        cmap.map_bf_range(low, high, dst_low)?;
                    }
                    ref token => {
                        if let Some(dst_str) = expect_string(token) {
                            cmap.map_bf_range(low, high, dst_str)?;
                        } else if let Token::Command(cmd) = token {
                            if cmd == "[" {
                                let mut array = Vec::new();
                                loop {
                                    let array_obj = lexer.get_obj();
                                    match array_obj {
                                        Token::Command(cmd) if cmd == "]" => break,
                                        Token::Eof => break,
                                        Token::Integer(val) => array.push(val as u32),
                                        ref arr_token => {
                                            if let Some(val_str) = expect_string(arr_token) {
                                                array.push(bf_string_char(&val_str));
                                            }
                                        }
                                    }
                                }
                                cmap.map_bf_range_to_array(low, high, array)?;
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    }
                }
            }
        }
    }

    Some(())
}

fn parse_cid_char(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    loop {
        let obj = lexer.get_obj();
        match obj {
            Token::Eof => break,
            Token::Command(cmd) if cmd == "endcidchar" => return Some(()),
            ref token => {
                let src_str = expect_string(token)?;
                let src = str_to_int(&src_str);
                let dst_obj = lexer.get_obj();
                let dst = expect_int(&dst_obj)?;
                cmap.map_one(src, dst as u32);
            }
        }
    }

    Some(())
}

fn parse_cid_range(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    loop {
        let obj = lexer.get_obj();
        match obj {
            Token::Eof => break,
            Token::Command(cmd) if cmd == "endcidrange" => return Some(()),
            ref token => {
                let low_str = expect_string(token)?;
                let low = str_to_int(&low_str);

                let high_obj = lexer.get_obj();
                let high_str = expect_string(&high_obj)?;
                let high = str_to_int(&high_str);

                let dst_obj = lexer.get_obj();
                let dst_low = expect_int(&dst_obj)?;

                cmap.map_cid_range(low, high, dst_low as u32)?;
            }
        }
    }

    Some(())
}

fn parse_codespace_range(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    loop {
        let obj = lexer.get_obj();
        match obj {
            Token::Eof => break,
            Token::Command(cmd) if cmd == "endcodespacerange" => return Some(()),
            ref token => {
                let low_str = expect_string(token)?;
                if low_str.is_empty() {
                    continue;
                }
                let low = str_to_int(&low_str);

                let high_obj = lexer.get_obj();
                let high_str = expect_string(&high_obj)?;
                if high_str.is_empty() {
                    return None;
                }
                let high = str_to_int(&high_str);

                cmap.add_codespace_range(high_str.chars().count(), low, high);
            }
        }
    }

    Some(())
}

fn parse_wmode(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    let obj = lexer.get_obj();
    if let Some(val) = expect_int(&obj) {
        cmap.vertical = val != 0;
    }

    Some(())
}

fn parse_cmap_name(cmap: &mut CMap, lexer: &mut CMapLexer) -> Option<()> {
    let obj = lexer.get_obj();
    match obj {
        Token::Name(name) => {
            cmap.name = name;
            Some(())
        }
        _ => Some(()), // Don't error on unexpected tokens, just ignore
    }
}

pub fn parse_cmap(input: &[u8]) -> Option<CMap> {
    let mut cmap = CMap::new();
    let mut lexer = CMapLexer::new(input);

    loop {
        let obj = lexer.get_obj();
        match obj {
            Token::Eof => break,
            Token::Name(ref name) => {
                if name == "WMode" {
                    parse_wmode(&mut cmap, &mut lexer)?;
                } else if name == "CMapName" {
                    parse_cmap_name(&mut cmap, &mut lexer)?;
                }
            }
            Token::Command(ref cmd) => {
                match cmd.as_str() {
                    "endcmap" => break,
                    "usecmap" => {
                        // TODO: Implement
                    }
                    "begincodespacerange" => {
                        parse_codespace_range(&mut cmap, &mut lexer)?;
                    }
                    "beginbfchar" => {
                        parse_bf_char(&mut cmap, &mut lexer)?;
                    }
                    "begincidchar" => {
                        parse_cid_char(&mut cmap, &mut lexer)?;
                    }
                    "beginbfrange" => {
                        parse_bf_range(&mut cmap, &mut lexer)?;
                    }
                    "begincidrange" => {
                        parse_cid_range(&mut cmap, &mut lexer)?;
                    }
                    "def" | "dict" | "begin" | "end" | "findresource" | "<<" | ">>" | "pop"
                    | "currentdict" | "defineresource" => {}
                    _ => {
                        // Skip any other unknown commands.
                    }
                }
            }
            Token::String(_) | Token::HexString(_) | Token::Integer(_) => {
                // Skip standalone tokens that aren't part of a command we recognize.
            }
        }
    }

    Some(cmap)
}

fn hex_to_int(a: &[u8], size: usize) -> u32 {
    let mut n = 0u32;
    for i in 0..=size {
        if i < a.len() {
            n = (n << 8) | a[i] as u32;
        }
    }
    n
}

fn hex_to_str(a: &[u8], size: usize) -> String {
    let bytes = if size + 1 <= a.len() { &a[0..=size] } else { a };

    String::from_utf8_lossy(bytes).to_string()
}

fn add_hex(a: &mut [u8], b: &[u8], size: usize) {
    let mut c = 0u32;
    for i in (0..=size).rev() {
        if i < a.len() && i < b.len() {
            c += a[i] as u32 + b[i] as u32;
            a[i] = (c & 255) as u8;
            c >>= 8;
        }
    }
}

fn inc_hex(a: &mut [u8], size: usize) {
    let mut c = 1u32;
    for i in (0..=size).rev() {
        if i < a.len() && c > 0 {
            c += a[i] as u32;
            a[i] = (c & 255) as u8;
            c >>= 8;
        }
    }
}

struct BinaryCMapStream {
    buffer: Vec<u8>,
    pos: usize,
    end: usize,
    tmp_buf: Vec<u8>,
}

impl BinaryCMapStream {
    fn new(data: Vec<u8>) -> Self {
        let end = data.len();
        Self {
            buffer: data,
            pos: 0,
            end,
            tmp_buf: vec![0; MAX_ENCODED_NUM_SIZE],
        }
    }

    fn read_byte(&mut self) -> i32 {
        if self.pos >= self.end {
            return -1;
        }
        let b = self.buffer[self.pos] as i32;
        self.pos += 1;
        b
    }

    fn read_number(&mut self) -> Result<u32, String> {
        let mut n = 0u32;
        let mut last;
        loop {
            let b = self.read_byte();
            if b < 0 {
                return Err("unexpected EOF in bcmap".to_string());
            }
            last = (b & 0x80) == 0;
            n = (n << 7) | ((b & 0x7f) as u32);
            if last {
                break;
            }
        }
        Ok(n)
    }

    fn read_signed(&mut self) -> Result<i32, String> {
        let n = self.read_number()?;
        Ok(if n & 1 != 0 {
            !((n >> 1) as i32)
        } else {
            (n >> 1) as i32
        })
    }

    fn read_hex(&mut self, num: &mut [u8], size: usize) -> Result<(), String> {
        if self.pos + size + 1 > self.end {
            return Err("unexpected EOF in bcmap".to_string());
        }
        let len = (size + 1).min(num.len());
        num[0..len].copy_from_slice(&self.buffer[self.pos..self.pos + len]);
        self.pos += size + 1;
        Ok(())
    }

    fn read_hex_number(&mut self, num: &mut [u8], size: usize) -> Result<(), String> {
        let mut last;
        let mut sp = 0;
        self.tmp_buf.clear();

        loop {
            let b = self.read_byte();
            if b < 0 {
                return Err("unexpected EOF in bcmap".to_string());
            }
            last = (b & 0x80) == 0;
            if sp < self.tmp_buf.capacity() {
                self.tmp_buf.push((b & 0x7f) as u8);
                sp += 1;
            }
            if last {
                break;
            }
        }

        let mut i = size as i32;
        let mut buffer = 0u32;
        let mut buffer_size: i32 = 0;

        while i >= 0 {
            while buffer_size < 8 && !self.tmp_buf.is_empty() {
                let val = self.tmp_buf.pop().unwrap() as u32;
                buffer |= val << buffer_size;
                buffer_size += 7;
            }
            if (i as usize) < num.len() {
                num[i as usize] = (buffer & 255) as u8;
            }
            i -= 1;
            buffer >>= 8;
            buffer_size = buffer_size.saturating_sub(8);
        }
        Ok(())
    }

    fn read_hex_signed(&mut self, num: &mut [u8], size: usize) -> Result<(), String> {
        self.read_hex_number(num, size)?;
        let sign = if size < num.len() && (num[size] & 1) != 0 {
            255
        } else {
            0
        };
        let mut c = 0u32;
        for i in 0..=size {
            if i < num.len() {
                c = ((c & 1) << 8) | num[i] as u32;
                num[i] = ((c >> 1) ^ sign as u32) as u8;
            }
        }
        Ok(())
    }

    fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_number()? as usize;
        let mut buf = Vec::with_capacity(len);
        for _ in 0..len {
            let val = self.read_number()?;
            if val <= u32::from(u8::MAX) {
                buf.push(val as u8);
            }
        }
        Ok(String::from_utf8_lossy(&buf).to_string())
    }
}

pub struct BinaryCMapReader;

impl BinaryCMapReader {
    pub fn new() -> Self {
        Self
    }

    pub fn process(&self, data: Vec<u8>, cmap: &mut CMap) -> Result<(), String> {
        let mut stream = BinaryCMapStream::new(data);
        let header = stream.read_byte();
        if header < 0 {
            return Err("unexpected EOF in bcmap header".to_string());
        }
        cmap.vertical = (header & 1) != 0;

        let mut start = vec![0u8; MAX_NUM_SIZE];
        let mut end = vec![0u8; MAX_NUM_SIZE];
        let mut char = vec![0u8; MAX_NUM_SIZE];
        let mut char_code = vec![0u8; MAX_NUM_SIZE];
        let mut tmp = vec![0u8; MAX_NUM_SIZE];

        loop {
            let b = stream.read_byte();
            if b < 0 {
                break; // EOF
            }

            let type_val = (b >> 5) & 0x7;
            if type_val == 7 {
                // metadata, e.g. comment or usecmap
                match b & 0x1f {
                    0 => {
                        stream.read_string()?; // skipping comment
                    }
                    1 => {
                        let _use_cmap = stream.read_string()?; // TODO: handle usecmap
                    }
                    _ => {}
                }
                continue;
            }

            let sequence = (b & 0x10) != 0;
            let data_size = (b & 15) as usize;

            if data_size + 1 > MAX_NUM_SIZE {
                return Err("BinaryCMapReader.process: Invalid dataSize.".to_string());
            }

            let ucs2_data_size = 1usize;
            let subitems_count = stream.read_number()? as usize;

            match type_val {
                0 => {
                    // codespacerange
                    stream.read_hex(&mut start, data_size)?;
                    stream.read_hex_number(&mut end, data_size)?;
                    add_hex(&mut end, &start, data_size);
                    cmap.add_codespace_range(
                        data_size + 1,
                        hex_to_int(&start, data_size),
                        hex_to_int(&end, data_size),
                    );
                    for _i in 1..subitems_count {
                        inc_hex(&mut end, data_size);
                        stream.read_hex_number(&mut start, data_size)?;
                        add_hex(&mut start, &end, data_size);
                        stream.read_hex_number(&mut end, data_size)?;
                        add_hex(&mut end, &start, data_size);
                        cmap.add_codespace_range(
                            data_size + 1,
                            hex_to_int(&start, data_size),
                            hex_to_int(&end, data_size),
                        );
                    }
                }
                1 => {
                    // notdefrange - skip undefined range
                    stream.read_hex(&mut start, data_size)?;
                    stream.read_hex_number(&mut end, data_size)?;
                    add_hex(&mut end, &start, data_size);
                    stream.read_number()?; // code
                    for _i in 1..subitems_count {
                        inc_hex(&mut end, data_size);
                        stream.read_hex_number(&mut start, data_size)?;
                        add_hex(&mut start, &end, data_size);
                        stream.read_hex_number(&mut end, data_size)?;
                        add_hex(&mut end, &start, data_size);
                        stream.read_number()?; // code
                    }
                }
                2 => {
                    // cidchar
                    stream.read_hex(&mut char, data_size)?;
                    let mut code = stream.read_number()?;
                    cmap.map_one(hex_to_int(&char, data_size), code);
                    for _i in 1..subitems_count {
                        inc_hex(&mut char, data_size);
                        if !sequence {
                            stream.read_hex_number(&mut tmp, data_size)?;
                            add_hex(&mut char, &tmp, data_size);
                        }
                        let delta = stream.read_signed()?;
                        code = ((code as i64) + (delta as i64) + 1) as u32;
                        cmap.map_one(hex_to_int(&char, data_size), code);
                    }
                }
                3 => {
                    // cidrange
                    stream.read_hex(&mut start, data_size)?;
                    stream.read_hex_number(&mut end, data_size)?;
                    add_hex(&mut end, &start, data_size);
                    let code = stream.read_number()?;
                    cmap.map_cid_range(
                        hex_to_int(&start, data_size),
                        hex_to_int(&end, data_size),
                        code,
                    );
                    for _i in 1..subitems_count {
                        inc_hex(&mut end, data_size);
                        if !sequence {
                            stream.read_hex_number(&mut start, data_size)?;
                            add_hex(&mut start, &end, data_size);
                        } else {
                            start.copy_from_slice(&end);
                        }
                        stream.read_hex_number(&mut end, data_size)?;
                        add_hex(&mut end, &start, data_size);
                        let code = stream.read_number()?;
                        cmap.map_cid_range(
                            hex_to_int(&start, data_size),
                            hex_to_int(&end, data_size),
                            code,
                        );
                    }
                }
                4 => {
                    // bfchar
                    stream.read_hex(&mut char, ucs2_data_size)?;
                    stream.read_hex(&mut char_code, data_size)?;
                    let src = hex_to_int(&char, ucs2_data_size);
                    let dst_str = hex_to_str(&char_code, data_size);
                    cmap.map_one(src, bf_string_char(&dst_str));
                    for _i in 1..subitems_count {
                        inc_hex(&mut char, ucs2_data_size);
                        if !sequence {
                            stream.read_hex_number(&mut tmp, ucs2_data_size)?;
                            add_hex(&mut char, &tmp, ucs2_data_size);
                        }
                        inc_hex(&mut char_code, data_size);
                        stream.read_hex_signed(&mut tmp, data_size)?;
                        add_hex(&mut char_code, &tmp, data_size);
                        let src = hex_to_int(&char, ucs2_data_size);
                        let dst_str = hex_to_str(&char_code, data_size);
                        cmap.map_one(src, bf_string_char(&dst_str));
                    }
                }
                5 => {
                    // bfrange
                    stream.read_hex(&mut start, ucs2_data_size)?;
                    stream.read_hex_number(&mut end, ucs2_data_size)?;
                    add_hex(&mut end, &start, ucs2_data_size);
                    stream.read_hex(&mut char_code, data_size)?;
                    let low = hex_to_int(&start, ucs2_data_size);
                    let high = hex_to_int(&end, ucs2_data_size);
                    let dst_low = hex_to_str(&char_code, data_size);
                    cmap.map_bf_range(low, high, dst_low);
                    for _i in 1..subitems_count {
                        inc_hex(&mut end, ucs2_data_size);
                        if !sequence {
                            stream.read_hex_number(&mut start, ucs2_data_size)?;
                            add_hex(&mut start, &end, ucs2_data_size);
                        } else {
                            start.copy_from_slice(&end);
                        }
                        stream.read_hex_number(&mut end, ucs2_data_size)?;
                        add_hex(&mut end, &start, ucs2_data_size);
                        stream.read_hex(&mut char_code, data_size)?;
                        let low = hex_to_int(&start, ucs2_data_size);
                        let high = hex_to_int(&end, ucs2_data_size);
                        let dst_low = hex_to_str(&char_code, data_size);
                        cmap.map_bf_range(low, high, dst_low);
                    }
                }
                _ => {
                    return Err(format!(
                        "BinaryCMapReader.process - unknown type: {}",
                        type_val
                    ));
                }
            }
        }

        Ok(())
    }
}

pub fn parse_binary_cmap(data: Vec<u8>) -> Option<CMap> {
    let mut cmap = CMap::new();
    let reader = BinaryCMapReader::new();

    match reader.process(data, &mut cmap) {
        Ok(()) => Some(cmap),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_beginbfchar() {
        let input = r#"2 beginbfchar
<03> <00>
<04> <01>
endbfchar"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        assert_eq!(cmap.lookup_code(0x03), Some(0x00));
        assert_eq!(cmap.lookup_code(0x04), Some(0x01));
        assert!(cmap.lookup_code(0x05).is_none());
    }

    #[test]
    fn test_parse_beginbfrange_with_range() {
        let input = r#"1 beginbfrange
<06> <0B> 0
endbfrange"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        assert!(cmap.lookup_code(0x05).is_none());
        assert_eq!(cmap.lookup_code(0x06), Some(0x00));
        assert_eq!(cmap.lookup_code(0x0b), Some(0x05));
        assert!(cmap.lookup_code(0x0c).is_none());
    }

    #[test]
    fn test_parse_beginbfrange_with_array() {
        let input = r#"1 beginbfrange
<0D> <12> [ 0 1 2 3 4 5 ]
endbfrange"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        assert!(cmap.lookup_code(0x0c).is_none());
        assert_eq!(cmap.lookup_code(0x0d), Some(0x00));
        assert_eq!(cmap.lookup_code(0x12), Some(0x05));
        assert!(cmap.lookup_code(0x13).is_none());
    }

    #[test]
    fn test_parse_begincidchar() {
        let input = r#"1 begincidchar
<14> 0
endcidchar"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        assert_eq!(cmap.lookup_code(0x14), Some(0x00));
        assert!(cmap.lookup_code(0x15).is_none());
    }

    #[test]
    fn test_parse_begincidrange() {
        let input = r#"1 begincidrange
<0016> <001B> 0
endcidrange"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        assert!(cmap.lookup_code(0x15).is_none());
        assert_eq!(cmap.lookup_code(0x16), Some(0x00));
        assert_eq!(cmap.lookup_code(0x1b), Some(0x05));
        assert!(cmap.lookup_code(0x1c).is_none());
    }

    #[test]
    fn test_parse_4_byte_codespace_ranges() {
        let input = r#"1 begincodespacerange
<8EA1A1A1> <8EA1FEFE>
endcodespacerange"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        let test_bytes = [0x8E, 0xA1, 0xA1, 0xA1];
        let (charcode, length) = cmap.read_code(&test_bytes, 0);
        assert_eq!(charcode, 0x8ea1a1a1);
        assert_eq!(length, 4);
    }

    #[test]
    fn test_parse_cmap_name() {
        let input = r#"/CMapName /Identity-H def"#.to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();
        assert_eq!(cmap.name, "Identity-H");
    }

    #[test]
    fn test_parse_wmode() {
        let input = r#"/WMode 1 def"#.to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();
        assert!(cmap.vertical);
    }

    #[test]
    fn test_identity_h_cmap() {
        let cmap = CMap::identity_h();

        assert_eq!(cmap.name, "Identity-H");
        assert!(!cmap.vertical);

        assert_eq!(cmap.lookup_code(0x41), Some(0x41));
        assert_eq!(cmap.lookup_code(0x1234), Some(0x1234));
        assert_eq!(cmap.lookup_code(0xFFFF), Some(0xFFFF));
        assert_eq!(cmap.lookup_code(0x10000), None);

        let test_bytes = [0x12, 0x34];
        let (charcode, length) = cmap.read_code(&test_bytes, 0);
        assert_eq!(charcode, 0x1234);
        assert_eq!(length, 2);
    }

    #[test]
    fn test_identity_v_cmap() {
        let cmap = CMap::identity_v();

        assert_eq!(cmap.name, "Identity-V");
        assert!(cmap.vertical);

        assert_eq!(cmap.lookup_code(0x41), Some(0x41));
        assert_eq!(cmap.lookup_code(0x1234), Some(0x1234));
        assert_eq!(cmap.lookup_code(0xFFFF), Some(0xFFFF));
        assert_eq!(cmap.lookup_code(0x10000), None);
    }

    #[test]
    fn test_simple_cidrange() {
        let input = r#"1 begincidrange
<00> <FF> 0
endcidrange"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        // Should map codes 0x00-0xFF to CIDs 0-255
        assert_eq!(cmap.lookup_code(0x00), Some(0));
        assert_eq!(cmap.lookup_code(0x41), Some(65));
        assert_eq!(cmap.lookup_code(0xFF), Some(255));
        assert_eq!(cmap.lookup_code(0x100), None);
    }

    #[test]
    fn test_complex_cmap_with_postscript() {
        let input = r#"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo
<< /Registry (Adobe)
/Ordering (Identity)
/Supplement 0
>> def
/CMapName /Identity-H def
/CMapType 2 def
1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidrange
<00> <FF> 0
endcidrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end"#
            .to_string();

        let cmap = parse_cmap(input.as_bytes()).unwrap();

        assert_eq!(cmap.lookup_code(0x00), Some(0));
        assert_eq!(cmap.lookup_code(0x41), Some(65));
        assert_eq!(cmap.lookup_code(0xFF), Some(255));
        assert_eq!(cmap.lookup_code(0x100), None);
        assert_eq!(cmap.name, "Identity-H");
    }

    #[test]
    fn test_parse_binary_cmap_adobe_japan1_ucs2() {
        let data = std::fs::read("assets/bcmaps/Adobe-Japan1-UCS2.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        // Test some known mappings from Adobe-Japan1-UCS2
        // These are sample mappings that should exist in the cmap
        assert!(cmap.lookup_code(0x20).is_some()); // Space character
        assert!(cmap.lookup_code(0x21).is_some()); // Exclamation mark

        // Test that the cmap is not empty
        assert!(cmap.map.len() > 0);

        // Test codespace ranges
        let test_bytes = [0x00, 0x20];
        let (charcode, length) = cmap.read_code(&test_bytes, 0);
        assert_eq!(length, 2);
        assert_eq!(charcode, 0x0020);
    }

    #[test]
    fn test_parse_binary_cmap_adobe_gb1_ucs2() {
        let data = std::fs::read("assets/bcmaps/Adobe-GB1-UCS2.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        // Test some known mappings from Adobe-GB1-UCS2
        assert!(cmap.lookup_code(0x20).is_some()); // Space character
        assert!(cmap.lookup_code(0x21).is_some()); // Exclamation mark

        // Test that the cmap is not empty
        assert!(cmap.map.len() > 0);
    }

    #[test]
    fn test_text_and_binary_cmap_parsing() {
        // Test that both text and binary parsers work on appropriate data

        // Test text parser with valid text CMAP
        let text_input = r#"1 begincodespacerange
<00> <FF>
endcodespacerange
1 begincidchar
<41> 65
endcidchar"#;
        let text_cmap = parse_cmap(text_input.as_bytes()).unwrap();
        assert_eq!(text_cmap.lookup_code(0x41), Some(65));

        // Test binary parser with binary CMAP data
        let binary_data = std::fs::read("assets/bcmaps/Adobe-Japan1-UCS2.bcmap").unwrap();
        let binary_cmap = parse_binary_cmap(binary_data).unwrap();
        assert!(binary_cmap.map.len() > 0);

        // This verifies that both parsing methods work correctly
        // The fallback logic in read_encoding will try text first, then binary
    }

    #[test]
    fn test_parse_binary_cmap_adobe_korea1_ucs2() {
        let data = std::fs::read("assets/bcmaps/Adobe-Korea1-UCS2.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        // Test some known mappings from Adobe-Korea1-UCS2
        assert!(cmap.lookup_code(0x20).is_some()); // Space character
        assert!(cmap.lookup_code(0x21).is_some()); // Exclamation mark

        // Test that the cmap is not empty
        assert!(cmap.map.len() > 0);

        // Test codespace ranges
        let test_bytes = [0x00, 0x20];
        let (charcode, length) = cmap.read_code(&test_bytes, 0);
        assert_eq!(length, 2);
        assert_eq!(charcode, 0x0020);
    }

    #[test]
    fn test_binary_cmap_stream() {
        // Test the BinaryCMapStream functionality
        let data = vec![0x01, 0x80, 0x02, 0x03]; // Sample binary data
        let mut stream = BinaryCMapStream::new(data);

        assert_eq!(stream.read_byte(), 1);
        assert_eq!(stream.read_byte(), 128);
        assert_eq!(stream.read_byte(), 2);
        assert_eq!(stream.read_byte(), 3);
        assert_eq!(stream.read_byte(), -1); // EOF
    }

    #[test]
    fn test_binary_cmap_number_reading() {
        // Test reading encoded numbers from binary stream
        // Single byte number (no continuation bit)
        let data = vec![0x01];
        let mut stream = BinaryCMapStream::new(data);
        let num = stream.read_number().unwrap();
        assert_eq!(num, 1);

        // Multi-byte number with continuation bit
        let data2 = vec![0x81, 0x01]; // First byte has continuation bit (0x80), value is (1<<7)|1 = 129
        let mut stream2 = BinaryCMapStream::new(data2);
        let num2 = stream2.read_number().unwrap();
        assert_eq!(num2, 129);
    }

    #[test]
    fn bcmap_adobe_gb1_ucs2() {
        let data = std::fs::read("assets/bcmaps/Adobe-GB1-UCS2.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        assert_eq!(cmap.lookup_code(0x3afa), Some(112));
        assert_eq!(cmap.lookup_code(0x2966), Some(81));
        assert_eq!(cmap.lookup_code(0x6946), Some(69));
        assert_eq!(cmap.lookup_code(0x69dc), Some(69));
        assert_eq!(cmap.lookup_code(0x1793), Some(90));
    }

    #[test]
    fn bcmap_adobe_japan1_ucs2() {
        let data = std::fs::read("assets/bcmaps/Adobe-Japan1-UCS2.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        assert_eq!(cmap.lookup_code(0x18ce), Some(65533));
        assert_eq!(cmap.lookup_code(0x20c3), Some(80));
        assert_eq!(cmap.lookup_code(0x1f1d), Some(114));
        assert_eq!(cmap.lookup_code(0x38e7), Some(99));
        assert_eq!(cmap.lookup_code(0x028b), Some(48));
    }

    #[test]
    fn bcmap_adobe_korea1_ucs2() {
        let data = std::fs::read("assets/bcmaps/Adobe-Korea1-UCS2.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        assert_eq!(cmap.lookup_code(0x0b05), Some(65533));
        assert_eq!(cmap.lookup_code(0x14ea), Some(123));
        assert_eq!(cmap.lookup_code(0x1ec7), Some(120));
        assert_eq!(cmap.lookup_code(0x4553), Some(1361));
        assert_eq!(cmap.lookup_code(0x148b), Some(91));
    }

    #[test]
    fn bcmap_78_rksj_h() {
        let data = std::fs::read("assets/bcmaps/78-RKSJ-H.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        assert_eq!(cmap.lookup_code(0x8a6d), Some(1452));
        assert_eq!(cmap.lookup_code(0x9bc3), Some(4690));
        assert_eq!(cmap.lookup_code(0x8fd0), Some(2490));
        assert_eq!(cmap.lookup_code(0x9052), Some(2553));
        assert_eq!(cmap.lookup_code(0x92f1), Some(3087));
    }

    #[test]
    fn bcmap_gb_h() {
        let data = std::fs::read("assets/bcmaps/GB-H.bcmap").unwrap();
        let cmap = parse_binary_cmap(data).unwrap();

        assert_eq!(cmap.lookup_code(0x265f), Some(579));
        assert_eq!(cmap.lookup_code(0x273f), Some(632));
        assert_eq!(cmap.lookup_code(0x5221), Some(4136));
        assert_eq!(cmap.lookup_code(0x754b), Some(7463));
        assert_eq!(cmap.lookup_code(0x3e24), Some(2259));
    }
}
