//! `zlib.deflateSync(buf)` exactly as Node produces it.
//!
//! Node bundles Chromium's zlib fork ("1.3.2.1-motley"), whose compressor
//! does not produce the same bytes as upstream zlib (and so not the same as
//! flate2 with any backend: miniz_oxide, zlib-rs or the system libz): it
//! hashes 4 bytes per position with a multiplicative hash
//! (`((u32le * 66521 + 66521) >> 16) & hash_mask`, `contrib/optimizations/
//! insert_string.h`) instead of zlib's 3-byte Rabin-Karp rolling hash, and
//! then never compares the third byte of a candidate match. Everything else
//! (lazy matching, the level-6 parameters, block splitting at 16383 symbols,
//! the Huffman trees) is upstream zlib 1.3.
//!
//! This is a straight port of that code path (deflate.c `deflate_slow`,
//! `longest_match`, `fill_window`; trees.c) for the one call icloud-md makes:
//! a single-shot zlib stream at the default level (6), windowBits 15,
//! memLevel 8, default strategy, flushed with Z_FINISH. The Chromium hash is
//! architecture independent (the SIMD paths only change speed), so the
//! output is the same on every machine Node runs on.
//!
//! Source: node v26.7.0 `deps/zlib` (zlib license - see NOTICE).

const W_BITS: u32 = 15;
const W_SIZE: usize = 1 << W_BITS;
const W_MASK: usize = W_SIZE - 1;
const WINDOW_SIZE: usize = 2 * W_SIZE;
/// Chromium allocates `2 * (w_size + WINDOW_PADDING)` zeroed bytes.
const WINDOW_ALLOC: usize = 2 * (W_SIZE + 8);
const HASH_BITS: u32 = 15;
const HASH_SIZE: usize = 1 << HASH_BITS;
const HASH_MASK: u32 = (HASH_SIZE - 1) as u32;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;
const MAX_DIST: usize = W_SIZE - MIN_LOOKAHEAD;
const WIN_INIT: usize = MAX_MATCH;
const TOO_FAR: usize = 4096;
const LIT_BUFSIZE: usize = 1 << (8 + 6);
const SYM_END: usize = LIT_BUFSIZE - 1;

// Level 6 of `configuration_table`.
const GOOD_MATCH: usize = 8;
const MAX_LAZY: usize = 16;
const NICE_MATCH: usize = 128;
const MAX_CHAIN: u32 = 128;

// trees.c
const LENGTH_CODES: usize = 29;
const LITERALS: usize = 256;
const L_CODES: usize = LITERALS + 1 + LENGTH_CODES;
const D_CODES: usize = 30;
const BL_CODES: usize = 19;
const HEAP_SIZE: usize = 2 * L_CODES + 1;
const MAX_BITS: usize = 15;
const MAX_BL_BITS: usize = 7;
const END_BLOCK: usize = 256;
const REP_3_6: usize = 16;
const REPZ_3_10: usize = 17;
const REPZ_11_138: usize = 18;

const EXTRA_LBITS: [u32; LENGTH_CODES] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const EXTRA_DBITS: [u32; D_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
];
const EXTRA_BLBITS: [u32; BL_CODES] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];
const BL_ORDER: [usize; BL_CODES] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// `ct_data`: `fc` is the Freq/Code union, `dl` the Dad/Len union - the
/// aliasing is load-bearing in `gen_bitlen`.
#[derive(Clone, Copy, Default)]
struct Ct {
    fc: u16,
    dl: u16,
}

struct StaticTables {
    ltree: [Ct; L_CODES + 2],
    dtree: [Ct; D_CODES],
    dist_code: [u8; 512],
    length_code: [u8; MAX_MATCH - MIN_MATCH + 1],
    base_length: [u32; LENGTH_CODES],
    base_dist: [u32; D_CODES],
}

fn bi_reverse(mut code: u32, mut len: u32) -> u32 {
    let mut res = 0u32;
    loop {
        res |= code & 1;
        code >>= 1;
        res <<= 1;
        len -= 1;
        if len == 0 {
            break;
        }
    }
    res >> 1
}

fn gen_codes(tree: &mut [Ct], max_code: isize, bl_count: &[u16; MAX_BITS + 1]) {
    let mut next_code = [0u16; MAX_BITS + 1];
    let mut code: u32 = 0;
    for bits in 1..=MAX_BITS {
        code = (code + u32::from(bl_count[bits - 1])) << 1;
        next_code[bits] = code as u16;
    }
    let mut n: isize = 0;
    while n <= max_code {
        let len = tree[n as usize].dl as usize;
        if len != 0 {
            tree[n as usize].fc = bi_reverse(u32::from(next_code[len]), len as u32) as u16;
            next_code[len] = next_code[len].wrapping_add(1);
        }
        n += 1;
    }
}

fn static_tables() -> &'static StaticTables {
    static TABLES: std::sync::OnceLock<StaticTables> = std::sync::OnceLock::new();
    TABLES.get_or_init(|| {
        let mut t = StaticTables {
            ltree: [Ct::default(); L_CODES + 2],
            dtree: [Ct::default(); D_CODES],
            dist_code: [0; 512],
            length_code: [0; MAX_MATCH - MIN_MATCH + 1],
            base_length: [0; LENGTH_CODES],
            base_dist: [0; D_CODES],
        };
        let mut length = 0usize;
        let mut code = 0usize;
        while code < LENGTH_CODES - 1 {
            t.base_length[code] = length as u32;
            for _ in 0..(1usize << EXTRA_LBITS[code]) {
                t.length_code[length] = code as u8;
                length += 1;
            }
            code += 1;
        }
        t.length_code[length - 1] = code as u8;
        let mut dist = 0usize;
        code = 0;
        while code < 16 {
            t.base_dist[code] = dist as u32;
            for _ in 0..(1usize << EXTRA_DBITS[code]) {
                t.dist_code[dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        dist >>= 7;
        while code < D_CODES {
            t.base_dist[code] = (dist << 7) as u32;
            for _ in 0..(1usize << (EXTRA_DBITS[code] - 7)) {
                t.dist_code[256 + dist] = code as u8;
                dist += 1;
            }
            code += 1;
        }
        let mut bl_count = [0u16; MAX_BITS + 1];
        for n in 0..=287usize {
            let len = match n {
                0..=143 => 8,
                144..=255 => 9,
                256..=279 => 7,
                _ => 8,
            };
            t.ltree[n].dl = len;
            bl_count[len as usize] += 1;
        }
        gen_codes(&mut t.ltree, (L_CODES + 1) as isize, &bl_count);
        for n in 0..D_CODES {
            t.dtree[n].dl = 5;
            t.dtree[n].fc = bi_reverse(n as u32, 5) as u16;
        }
        t
    })
}

fn d_code(t: &StaticTables, dist: usize) -> usize {
    if dist < 256 {
        t.dist_code[dist] as usize
    } else {
        t.dist_code[256 + (dist >> 7)] as usize
    }
}

struct StaticDesc {
    stree: Option<&'static [Ct]>,
    extra_bits: &'static [u32],
    extra_base: usize,
    elems: usize,
    max_length: usize,
}

/// Heap and bit-length bookkeeping shared by the three trees.
struct TreeState {
    heap: [i32; HEAP_SIZE],
    heap_len: usize,
    heap_max: usize,
    depth: [u8; HEAP_SIZE],
    bl_count: [u16; MAX_BITS + 1],
    opt_len: u64,
    static_len: u64,
}

impl TreeState {
    fn smaller(&self, tree: &[Ct], n: usize, m: usize) -> bool {
        tree[n].fc < tree[m].fc || (tree[n].fc == tree[m].fc && self.depth[n] <= self.depth[m])
    }

    fn pqdownheap(&mut self, tree: &[Ct], mut k: usize) {
        let v = self.heap[k] as usize;
        let mut j = k << 1;
        while j <= self.heap_len {
            if j < self.heap_len && self.smaller(tree, self.heap[j + 1] as usize, self.heap[j] as usize) {
                j += 1;
            }
            if self.smaller(tree, v, self.heap[j] as usize) {
                break;
            }
            self.heap[k] = self.heap[j];
            k = j;
            j <<= 1;
        }
        self.heap[k] = v as i32;
    }

    fn gen_bitlen(&mut self, tree: &mut [Ct], max_code: isize, desc: &StaticDesc) {
        let max_length = desc.max_length;
        for c in self.bl_count.iter_mut() {
            *c = 0;
        }
        tree[self.heap[self.heap_max] as usize].dl = 0;
        let mut overflow = 0i32;
        let mut h = self.heap_max + 1;
        while h < HEAP_SIZE {
            let n = self.heap[h] as usize;
            let mut bits = tree[tree[n].dl as usize].dl as usize + 1;
            if bits > max_length {
                bits = max_length;
                overflow += 1;
            }
            tree[n].dl = bits as u16;
            h += 1;
            if n as isize > max_code {
                continue;
            }
            self.bl_count[bits] += 1;
            let mut xbits = 0u64;
            if n >= desc.extra_base {
                xbits = u64::from(desc.extra_bits[n - desc.extra_base]);
            }
            let f = u64::from(tree[n].fc);
            self.opt_len = self.opt_len.wrapping_add(f * (bits as u64 + xbits));
            if let Some(stree) = desc.stree {
                self.static_len = self.static_len.wrapping_add(f * (u64::from(stree[n].dl) + xbits));
            }
        }
        if overflow == 0 {
            return;
        }
        loop {
            let mut bits = max_length - 1;
            while self.bl_count[bits] == 0 {
                bits -= 1;
            }
            self.bl_count[bits] -= 1;
            self.bl_count[bits + 1] += 2;
            self.bl_count[max_length] -= 1;
            overflow -= 2;
            if overflow <= 0 {
                break;
            }
        }
        let mut h = HEAP_SIZE;
        let mut bits = max_length;
        while bits != 0 {
            let mut n = self.bl_count[bits];
            while n != 0 {
                h -= 1;
                let m = self.heap[h] as usize;
                if m as isize > max_code {
                    continue;
                }
                if tree[m].dl as usize != bits {
                    let delta = (bits as u64).wrapping_sub(u64::from(tree[m].dl));
                    self.opt_len = self.opt_len.wrapping_add(delta.wrapping_mul(u64::from(tree[m].fc)));
                    tree[m].dl = bits as u16;
                }
                n -= 1;
            }
            bits -= 1;
        }
    }

    /// `build_tree`; returns the tree's `max_code`.
    #[allow(clippy::needless_range_loop)] // index-for-index port of trees.c
    fn build_tree(&mut self, tree: &mut [Ct], desc: &StaticDesc) -> isize {
        let elems = desc.elems;
        let mut max_code: isize = -1;
        self.heap_len = 0;
        self.heap_max = HEAP_SIZE;
        for n in 0..elems {
            if tree[n].fc != 0 {
                self.heap_len += 1;
                self.heap[self.heap_len] = n as i32;
                max_code = n as isize;
                self.depth[n] = 0;
            } else {
                tree[n].dl = 0;
            }
        }
        while self.heap_len < 2 {
            let node = if max_code < 2 {
                max_code += 1;
                max_code as usize
            } else {
                0
            };
            self.heap_len += 1;
            self.heap[self.heap_len] = node as i32;
            tree[node].fc = 1;
            self.depth[node] = 0;
            self.opt_len = self.opt_len.wrapping_sub(1);
            if let Some(stree) = desc.stree {
                self.static_len = self.static_len.wrapping_sub(u64::from(stree[node].dl));
            }
        }
        let mut n = self.heap_len / 2;
        while n >= 1 {
            self.pqdownheap(tree, n);
            n -= 1;
        }
        let mut node = elems;
        loop {
            // pqremove
            let n = self.heap[1] as usize;
            self.heap[1] = self.heap[self.heap_len];
            self.heap_len -= 1;
            self.pqdownheap(tree, 1);
            let m = self.heap[1] as usize;
            self.heap_max -= 1;
            self.heap[self.heap_max] = n as i32;
            self.heap_max -= 1;
            self.heap[self.heap_max] = m as i32;
            tree[node].fc = tree[n].fc.wrapping_add(tree[m].fc);
            self.depth[node] = self.depth[n].max(self.depth[m]) + 1;
            tree[n].dl = node as u16;
            tree[m].dl = node as u16;
            self.heap[1] = node as i32;
            node += 1;
            self.pqdownheap(tree, 1);
            if self.heap_len < 2 {
                break;
            }
        }
        self.heap_max -= 1;
        self.heap[self.heap_max] = self.heap[1];
        self.gen_bitlen(tree, max_code, desc);
        let bl_count = self.bl_count;
        gen_codes(tree, max_code, &bl_count);
        max_code
    }
}

/// LSB-first bit writer (`send_bits` / `bi_windup`).
struct BitWriter {
    out: Vec<u8>,
    bi_buf: u64,
    bi_valid: u32,
}

impl BitWriter {
    fn send_bits(&mut self, value: u32, length: u32) {
        self.bi_buf |= u64::from(value) << self.bi_valid;
        self.bi_valid += length;
        while self.bi_valid >= 8 {
            self.out.push(self.bi_buf as u8);
            self.bi_buf >>= 8;
            self.bi_valid -= 8;
        }
    }

    fn send_code(&mut self, c: usize, tree: &[Ct]) {
        self.send_bits(u32::from(tree[c].fc), u32::from(tree[c].dl));
    }

    fn bi_windup(&mut self) {
        if self.bi_valid > 0 {
            self.out.push(self.bi_buf as u8);
        }
        self.bi_buf = 0;
        self.bi_valid = 0;
    }
}

struct Deflater<'a> {
    input: &'a [u8],
    next_in: usize,
    window: Vec<u8>,
    prev: Vec<u16>,
    head: Vec<u16>,
    ins_h: u32,
    block_start: i64,
    match_length: usize,
    prev_match: usize,
    match_available: bool,
    strstart: usize,
    match_start: usize,
    lookahead: usize,
    prev_length: usize,
    insert: usize,
    high_water: usize,
    dyn_ltree: [Ct; HEAP_SIZE],
    dyn_dtree: [Ct; 2 * D_CODES + 1],
    bl_tree: [Ct; 2 * BL_CODES + 1],
    ts: TreeState,
    d_buf: Vec<u16>,
    l_buf: Vec<u8>,
    sym_next: usize,
    bits: BitWriter,
    adler_a: u32,
    adler_b: u32,
}

impl<'a> Deflater<'a> {
    fn new(input: &'a [u8]) -> Self {
        let mut d = Deflater {
            input,
            next_in: 0,
            window: vec![0; WINDOW_ALLOC],
            prev: vec![0; W_SIZE],
            head: vec![0; HASH_SIZE],
            ins_h: 0,
            block_start: 0,
            match_length: MIN_MATCH - 1,
            prev_match: 0,
            match_available: false,
            strstart: 0,
            match_start: 0,
            lookahead: 0,
            prev_length: MIN_MATCH - 1,
            insert: 0,
            high_water: 0,
            dyn_ltree: [Ct::default(); HEAP_SIZE],
            dyn_dtree: [Ct::default(); 2 * D_CODES + 1],
            bl_tree: [Ct::default(); 2 * BL_CODES + 1],
            ts: TreeState {
                heap: [0; HEAP_SIZE],
                heap_len: 0,
                heap_max: 0,
                depth: [0; HEAP_SIZE],
                bl_count: [0; MAX_BITS + 1],
                opt_len: 0,
                static_len: 0,
            },
            d_buf: vec![0; LIT_BUFSIZE],
            l_buf: vec![0; LIT_BUFSIZE],
            sym_next: 0,
            bits: BitWriter {
                out: Vec::new(),
                bi_buf: 0,
                bi_valid: 0,
            },
            adler_a: 1,
            adler_b: 0,
        };
        d.init_block();
        d
    }

    fn init_block(&mut self) {
        for n in 0..L_CODES {
            self.dyn_ltree[n].fc = 0;
        }
        for n in 0..D_CODES {
            self.dyn_dtree[n].fc = 0;
        }
        for n in 0..BL_CODES {
            self.bl_tree[n].fc = 0;
        }
        self.dyn_ltree[END_BLOCK].fc = 1;
        self.ts.opt_len = 0;
        self.ts.static_len = 0;
        self.sym_next = 0;
    }

    /// Chromium's `insert_string`.
    fn insert_string(&mut self, s: usize) -> usize {
        let value = u32::from_le_bytes([
            self.window[s],
            self.window[s + 1],
            self.window[s + 2],
            self.window[s + 3],
        ]);
        self.ins_h = (value.wrapping_mul(66521).wrapping_add(66521) >> 16) & HASH_MASK;
        let h = self.ins_h as usize;
        let ret = self.head[h];
        self.prev[s & W_MASK] = ret;
        self.head[h] = s as u16;
        ret as usize
    }

    fn slide_hash(&mut self) {
        for m in self.head.iter_mut() {
            *m = if usize::from(*m) >= W_SIZE {
                *m - W_SIZE as u16
            } else {
                0
            };
        }
        for m in self.prev.iter_mut() {
            *m = if usize::from(*m) >= W_SIZE {
                *m - W_SIZE as u16
            } else {
                0
            };
        }
    }

    fn read_buf(&mut self, at: usize, size: usize) -> usize {
        let len = (self.input.len() - self.next_in).min(size);
        if len == 0 {
            return 0;
        }
        let chunk = &self.input[self.next_in..self.next_in + len];
        self.window[at..at + len].copy_from_slice(chunk);
        for &byte in chunk {
            self.adler_a = (self.adler_a + u32::from(byte)) % 65521;
            self.adler_b = (self.adler_b + self.adler_a) % 65521;
        }
        self.next_in += len;
        len
    }

    fn fill_window(&mut self) {
        loop {
            let mut more = WINDOW_SIZE - self.lookahead - self.strstart;
            if self.strstart >= W_SIZE + MAX_DIST {
                self.window.copy_within(W_SIZE..W_SIZE + (W_SIZE - more), 0);
                self.match_start = self.match_start.wrapping_sub(W_SIZE);
                self.strstart -= W_SIZE;
                self.block_start -= W_SIZE as i64;
                if self.insert > self.strstart {
                    self.insert = self.strstart;
                }
                self.slide_hash();
                more += W_SIZE;
            }
            if self.next_in == self.input.len() {
                break;
            }
            let n = self.read_buf(self.strstart + self.lookahead, more);
            self.lookahead += n;
            if self.lookahead + self.insert > MIN_MATCH {
                let mut s = self.strstart - self.insert;
                while self.insert > 0 {
                    self.insert_string(s);
                    s += 1;
                    self.insert -= 1;
                    if self.lookahead + self.insert <= MIN_MATCH {
                        break;
                    }
                }
            }
            if !(self.lookahead < MIN_LOOKAHEAD && self.next_in != self.input.len()) {
                break;
            }
        }
        if self.high_water < WINDOW_SIZE {
            let curr = self.strstart + self.lookahead;
            if self.high_water < curr {
                let init = (WINDOW_SIZE - curr).min(WIN_INIT);
                self.window[curr..curr + init].fill(0);
                self.high_water = curr + init;
            } else if self.high_water < curr + WIN_INIT {
                let init = (curr + WIN_INIT - self.high_water).min(WINDOW_SIZE - self.high_water);
                self.window[self.high_water..self.high_water + init].fill(0);
                self.high_water += init;
            }
        }
    }

    fn longest_match(&mut self, mut cur_match: usize) -> usize {
        let mut chain_length = MAX_CHAIN;
        let scan = self.strstart;
        let mut best_len = self.prev_length;
        let mut nice_match = NICE_MATCH;
        let limit = self.strstart.saturating_sub(MAX_DIST);
        let w = &self.window;
        let mut scan_end1 = w[scan + best_len - 1];
        let mut scan_end = w[scan + best_len];
        if self.prev_length >= GOOD_MATCH {
            chain_length >>= 2;
        }
        if nice_match > self.lookahead {
            nice_match = self.lookahead;
        }
        loop {
            let m = cur_match;
            let candidate = w[m + best_len] == scan_end
                && w[m + best_len - 1] == scan_end1
                && w[m] == w[scan]
                && w[m + 1] == w[scan + 1];
            if candidate {
                // Byte 2 is never compared (zlib's rolling hash implied it;
                // Chromium's hash doesn't, and Chromium keeps the skip).
                let mut p = 3;
                while p < MAX_MATCH && w[scan + p] == w[m + p] {
                    p += 1;
                }
                let len = p;
                if len > best_len {
                    self.match_start = cur_match;
                    best_len = len;
                    if len >= nice_match {
                        break;
                    }
                    scan_end1 = w[scan + best_len - 1];
                    scan_end = w[scan + best_len];
                }
            }
            cur_match = usize::from(self.prev[cur_match & W_MASK]);
            if cur_match <= limit {
                break;
            }
            chain_length -= 1;
            if chain_length == 0 {
                break;
            }
        }
        best_len.min(self.lookahead)
    }

    fn tally_lit(&mut self, c: u8) -> bool {
        self.d_buf[self.sym_next] = 0;
        self.l_buf[self.sym_next] = c;
        self.sym_next += 1;
        self.dyn_ltree[usize::from(c)].fc += 1;
        self.sym_next == SYM_END
    }

    fn tally_dist(&mut self, distance: usize, length: usize) -> bool {
        let t = static_tables();
        let len = length as u8;
        let dist = distance as u16;
        self.d_buf[self.sym_next] = dist;
        self.l_buf[self.sym_next] = len;
        self.sym_next += 1;
        let dist = usize::from(dist.wrapping_sub(1));
        self.dyn_ltree[usize::from(t.length_code[usize::from(len)]) + LITERALS + 1].fc += 1;
        self.dyn_dtree[d_code(t, dist)].fc += 1;
        self.sym_next == SYM_END
    }

    fn flush_block(&mut self, last: bool) {
        let stored_len = (self.strstart as i64 - self.block_start) as usize;
        let buf_start = if self.block_start >= 0 {
            Some(self.block_start as usize)
        } else {
            None
        };
        self.tr_flush_block(buf_start, stored_len, last);
        self.block_start = self.strstart as i64;
    }

    fn scan_tree(bl_tree: &mut [Ct], tree: &mut [Ct], max_code: isize) {
        let mut prevlen: i32 = -1;
        let mut nextlen = i32::from(tree[0].dl);
        let mut count = 0i32;
        let mut max_count = 7;
        let mut min_count = 4;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        tree[(max_code + 1) as usize].dl = 0xffff;
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = i32::from(tree[(n + 1) as usize].dl);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                bl_tree[curlen as usize].fc = bl_tree[curlen as usize].fc.wrapping_add(count as u16);
            } else if curlen != 0 {
                if curlen != prevlen {
                    bl_tree[curlen as usize].fc += 1;
                }
                bl_tree[REP_3_6].fc += 1;
            } else if count <= 10 {
                bl_tree[REPZ_3_10].fc += 1;
            } else {
                bl_tree[REPZ_11_138].fc += 1;
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn send_tree(bits: &mut BitWriter, bl_tree: &[Ct], tree: &[Ct], max_code: isize) {
        let mut prevlen: i32 = -1;
        let mut nextlen = i32::from(tree[0].dl);
        let mut count = 0i32;
        let mut max_count = 7;
        let mut min_count = 4;
        if nextlen == 0 {
            max_count = 138;
            min_count = 3;
        }
        for n in 0..=max_code {
            let curlen = nextlen;
            nextlen = i32::from(tree[(n + 1) as usize].dl);
            count += 1;
            if count < max_count && curlen == nextlen {
                continue;
            } else if count < min_count {
                loop {
                    bits.send_code(curlen as usize, bl_tree);
                    count -= 1;
                    if count == 0 {
                        break;
                    }
                }
            } else if curlen != 0 {
                if curlen != prevlen {
                    bits.send_code(curlen as usize, bl_tree);
                    count -= 1;
                }
                bits.send_code(REP_3_6, bl_tree);
                bits.send_bits((count - 3) as u32, 2);
            } else if count <= 10 {
                bits.send_code(REPZ_3_10, bl_tree);
                bits.send_bits((count - 3) as u32, 3);
            } else {
                bits.send_code(REPZ_11_138, bl_tree);
                bits.send_bits((count - 11) as u32, 7);
            }
            count = 0;
            prevlen = curlen;
            if nextlen == 0 {
                max_count = 138;
                min_count = 3;
            } else if curlen == nextlen {
                max_count = 6;
                min_count = 3;
            } else {
                max_count = 7;
                min_count = 4;
            }
        }
    }

    fn compress_block(&mut self, ltree: &[Ct], dtree: &[Ct]) {
        let t = static_tables();
        for sx in 0..self.sym_next {
            let dist = usize::from(self.d_buf[sx]);
            let mut lc = usize::from(self.l_buf[sx]);
            if dist == 0 {
                self.bits.send_code(lc, ltree);
            } else {
                let code = usize::from(t.length_code[lc]);
                self.bits.send_code(code + LITERALS + 1, ltree);
                let extra = EXTRA_LBITS[code];
                if extra != 0 {
                    lc -= t.base_length[code] as usize;
                    self.bits.send_bits(lc as u32, extra);
                }
                let mut dist = dist - 1;
                let code = d_code(t, dist);
                self.bits.send_code(code, dtree);
                let extra = EXTRA_DBITS[code];
                if extra != 0 {
                    dist -= t.base_dist[code] as usize;
                    self.bits.send_bits(dist as u32, extra);
                }
            }
        }
        self.bits.send_code(END_BLOCK, ltree);
    }

    fn tr_flush_block(&mut self, buf_start: Option<usize>, stored_len: usize, last: bool) {
        let t = static_tables();
        let l_desc = StaticDesc {
            stree: Some(&t.ltree),
            extra_bits: &EXTRA_LBITS,
            extra_base: LITERALS + 1,
            elems: L_CODES,
            max_length: MAX_BITS,
        };
        let d_desc = StaticDesc {
            stree: Some(&t.dtree),
            extra_bits: &EXTRA_DBITS,
            extra_base: 0,
            elems: D_CODES,
            max_length: MAX_BITS,
        };
        let bl_desc = StaticDesc {
            stree: None,
            extra_bits: &EXTRA_BLBITS,
            extra_base: 0,
            elems: BL_CODES,
            max_length: MAX_BL_BITS,
        };
        let l_max = self.ts.build_tree(&mut self.dyn_ltree, &l_desc);
        let d_max = self.ts.build_tree(&mut self.dyn_dtree, &d_desc);
        // build_bl_tree
        Self::scan_tree(&mut self.bl_tree, &mut self.dyn_ltree, l_max);
        Self::scan_tree(&mut self.bl_tree, &mut self.dyn_dtree, d_max);
        self.ts.build_tree(&mut self.bl_tree, &bl_desc);
        let mut max_blindex = BL_CODES - 1;
        while max_blindex >= 3 {
            if self.bl_tree[BL_ORDER[max_blindex]].dl != 0 {
                break;
            }
            max_blindex -= 1;
        }
        self.ts.opt_len = self.ts.opt_len.wrapping_add(3 * (max_blindex as u64 + 1) + 5 + 5 + 4);

        let mut opt_lenb = self.ts.opt_len.wrapping_add(3 + 7) >> 3;
        let static_lenb = self.ts.static_len.wrapping_add(3 + 7) >> 3;
        if static_lenb <= opt_lenb {
            opt_lenb = static_lenb;
        }
        let last_bit = u32::from(last);
        if stored_len as u64 + 4 <= opt_lenb && buf_start.is_some() {
            let start = buf_start.unwrap_or(0);
            self.bits.send_bits(last_bit, 3);
            self.bits.bi_windup();
            let len = stored_len as u16;
            self.bits.out.extend_from_slice(&len.to_le_bytes());
            self.bits.out.extend_from_slice(&(!len).to_le_bytes());
            self.bits.out.extend_from_slice(&self.window[start..start + stored_len]);
        } else if static_lenb == opt_lenb {
            self.bits.send_bits((1 << 1) + last_bit, 3);
            self.compress_block(&t.ltree, &t.dtree);
        } else {
            self.bits.send_bits((2 << 1) + last_bit, 3);
            let lcodes = (l_max + 1) as u32;
            let dcodes = (d_max + 1) as u32;
            let blcodes = (max_blindex + 1) as u32;
            self.bits.send_bits(lcodes - 257, 5);
            self.bits.send_bits(dcodes - 1, 5);
            self.bits.send_bits(blcodes - 4, 4);
            for &code in BL_ORDER.iter().take(blcodes as usize) {
                self.bits.send_bits(u32::from(self.bl_tree[code].dl), 3);
            }
            Self::send_tree(&mut self.bits, &self.bl_tree, &self.dyn_ltree, lcodes as isize - 1);
            Self::send_tree(&mut self.bits, &self.bl_tree, &self.dyn_dtree, dcodes as isize - 1);
            let ltree = self.dyn_ltree;
            let dtree = self.dyn_dtree;
            self.compress_block(&ltree, &dtree);
        }
        self.init_block();
        if last {
            self.bits.bi_windup();
        }
    }

    /// `deflate_slow(s, Z_FINISH)`, run to completion.
    fn deflate_slow(&mut self) {
        loop {
            if self.lookahead < MIN_LOOKAHEAD {
                self.fill_window();
                if self.lookahead == 0 {
                    break;
                }
            }
            let mut hash_head = 0usize;
            if self.lookahead >= MIN_MATCH {
                hash_head = self.insert_string(self.strstart);
            }
            self.prev_length = self.match_length;
            self.prev_match = self.match_start;
            self.match_length = MIN_MATCH - 1;

            if hash_head != 0 && self.prev_length < MAX_LAZY && self.strstart - hash_head <= MAX_DIST {
                self.match_length = self.longest_match(hash_head);
                if self.match_length <= 5
                    && self.match_length == MIN_MATCH
                    && self.strstart - self.match_start > TOO_FAR
                {
                    self.match_length = MIN_MATCH - 1;
                }
            }
            if self.prev_length >= MIN_MATCH && self.match_length <= self.prev_length {
                let max_insert = self.strstart + self.lookahead - MIN_MATCH;
                let bflush = self.tally_dist(self.strstart - 1 - self.prev_match, self.prev_length - MIN_MATCH);
                self.lookahead -= self.prev_length - 1;
                self.prev_length -= 2;
                loop {
                    self.strstart += 1;
                    if self.strstart <= max_insert {
                        self.insert_string(self.strstart);
                    }
                    self.prev_length -= 1;
                    if self.prev_length == 0 {
                        break;
                    }
                }
                self.match_available = false;
                self.match_length = MIN_MATCH - 1;
                self.strstart += 1;
                if bflush {
                    self.flush_block(false);
                }
            } else if self.match_available {
                let bflush = self.tally_lit(self.window[self.strstart - 1]);
                if bflush {
                    self.flush_block(false);
                }
                self.strstart += 1;
                self.lookahead -= 1;
            } else {
                self.match_available = true;
                self.strstart += 1;
                self.lookahead -= 1;
            }
        }
        if self.match_available {
            self.tally_lit(self.window[self.strstart - 1]);
            self.match_available = false;
        }
        self.insert = self.strstart.min(MIN_MATCH - 1);
        self.flush_block(true);
    }
}

/// Node's `zlib.deflateSync(input)` (default options), byte for byte.
pub fn deflate_sync(input: &[u8]) -> Vec<u8> {
    let mut d = Deflater::new(input);
    // zlib header: CM=8, CINFO=7, level 6 -> FLEVEL 2, FCHECK.
    d.bits.out.extend_from_slice(&[0x78, 0x9c]);
    d.deflate_slow();
    let adler = (d.adler_b << 16) | d.adler_a;
    d.bits.out.extend_from_slice(&adler.to_be_bytes());
    d.bits.out
}
