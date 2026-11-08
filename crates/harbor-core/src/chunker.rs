//! 内容定义分块（CDC）与 BLAKE3 内容哈希。
//!
//! 采用 FastCDC 的齿轮哈希（Gear hash）+ 归一化掩码思路：
//! - 最小块 64 KiB、平均块 1 MiB、最大块 4 MiB；
//! - 只依赖滚动窗口内的字节，插入/删除局部数据只会影响相邻块；
//! - 文件内容哈希 = BLAKE3(所有块哈希按顺序拼接)，便于断点续传与跨文件去重。
//!
//! 这里刻意不引入 C/汇编实现之外的平台相关代码，所有平台使用同一张齿轮表，
//! 同一份内容在 Windows / macOS / Linux 上会得到完全相同的分块。

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::OnceLock;

/// 最小块大小：64 KiB。
pub const CHUNK_MIN: usize = 64 * 1024;
/// 平均块大小：1 MiB。
pub const CHUNK_AVG: usize = 1024 * 1024;
/// 最大块大小：4 MiB。
pub const CHUNK_MAX: usize = 4 * 1024 * 1024;

const READ_BUF: usize = 1024 * 1024;

/// 一个分块的元数据。`offset` 为文件内字节偏移，`hash` 为块内容 BLAKE3。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub offset: u64,
    pub len: u32,
    pub hash: [u8; 32],
}

impl Chunk {
    pub fn end(&self) -> u64 {
        self.offset + self.len as u64
    }
}

#[derive(Debug, Clone)]
pub struct FileChunks {
    pub size: u64,
    pub chunks: Vec<Chunk>,
    pub content_hash: [u8; 32],
}

impl FileChunks {
    pub fn chunk_count(&self) -> u32 {
        self.chunks.len() as u32
    }
}

/// 单块哈希。
pub fn chunk_hash(data: &[u8]) -> [u8; 32] {
    *blake3::hash(data).as_bytes()
}

/// 由块哈希序列计算文件内容哈希。
pub fn content_hash<'a>(hashes: impl IntoIterator<Item = &'a [u8; 32]>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for hash in hashes {
        hasher.update(hash);
    }
    *hasher.finalize().as_bytes()
}

/// 计算一段字节的 FastCDC 分块；返回块元数据与文件内容哈希。
pub fn chunk_bytes(data: &[u8]) -> FileChunks {
    let mut chunks = Vec::new();
    let mut offset = 0usize;
    let mut h = 0u64;
    let mut start = 0usize;

    for (index, byte) in data.iter().enumerate() {
        h = h.wrapping_shl(1).wrapping_add(gear()[*byte as usize]);
        let len = index + 1 - start;
        if len >= CHUNK_MIN {
            let mask = if len < CHUNK_AVG {
                mask_hard()
            } else {
                mask_easy()
            };
            if h & mask == 0 || len >= CHUNK_MAX {
                push_chunk(&mut chunks, &data[start..index + 1], offset);
                offset += len;
                start = index + 1;
                h = 0;
            }
        }
    }
    if start < data.len() {
        push_chunk(&mut chunks, &data[start..], offset);
    }

    let content = content_hash(chunks.iter().map(|chunk| &chunk.hash));
    FileChunks {
        size: data.len() as u64,
        chunks,
        content_hash: content,
    }
}

/// 流式分块一个文件。避免把大文件整体读入内存。
pub fn chunk_file(path: &Path) -> io::Result<FileChunks> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    chunk_reader(file, size)
}

/// 流式分块任意 reader。`size` 用于校验读取长度与块偏移。
pub fn chunk_reader<R: Read>(mut reader: R, size: u64) -> io::Result<FileChunks> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut offset = 0u64;
    let mut buffer = vec![0u8; READ_BUF];
    let mut current: Vec<u8> = Vec::with_capacity(CHUNK_MAX);
    let mut hash = 0u64;
    let mut read_total = 0u64;

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        read_total += n as u64;
        let mut start = 0usize;
        while start < n {
            let byte = buffer[start];
            current.push(byte);
            hash = hash.wrapping_shl(1).wrapping_add(gear()[byte as usize]);
            start += 1;

            if current.len() >= CHUNK_MIN {
                let mask = if current.len() < CHUNK_AVG {
                    mask_hard()
                } else {
                    mask_easy()
                };
                if hash & mask == 0 || current.len() >= CHUNK_MAX {
                    let len = current.len();
                    let chunk_hash = chunk_hash(&current);
                    chunks.push(Chunk {
                        offset,
                        len: len as u32,
                        hash: chunk_hash,
                    });
                    offset += len as u64;
                    current.clear();
                    hash = 0;
                }
            }
        }
    }

    if !current.is_empty() {
        let len = current.len();
        chunks.push(Chunk {
            offset,
            len: len as u32,
            hash: chunk_hash(&current),
        });
        offset += len as u64;
    }

    if read_total != size || offset != size {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("文件在读取过程中发生变化：预期 {size} 字节，实际读取 {read_total} 字节"),
        ));
    }

    let content = content_hash(chunks.iter().map(|chunk| &chunk.hash));
    Ok(FileChunks {
        size,
        chunks,
        content_hash: content,
    })
}

fn push_chunk(chunks: &mut Vec<Chunk>, data: &[u8], offset: usize) {
    chunks.push(Chunk {
        offset: offset as u64,
        len: data.len() as u32,
        hash: chunk_hash(data),
    });
}

/// 齿轮表使用固定种子的 SplitMix64 生成，跨平台稳定。
fn gear() -> &'static [u64; 256] {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut table = [0u64; 256];
        for slot in table.iter_mut() {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            *slot = z;
        }
        table
    })
}

fn mask_hard() -> u64 {
    // 1 MiB 平均块对应 20 位；前半段使用更难命中的 21 位掩码。
    (1u64 << 21) - 1
}

fn mask_easy() -> u64 {
    // 后半段使用更容易命中的 19 位掩码。两者组合后平均块约 1 MiB。
    (1u64 << 19) - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut state = seed;
        for _ in 0..len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((state >> 33) as u8);
        }
        out
    }

    #[test]
    fn empty_file_has_no_chunks() {
        let result = chunk_bytes(b"");
        assert!(result.chunks.is_empty());
        assert_eq!(result.content_hash, *blake3::hash(b"").as_bytes());
    }

    #[test]
    fn small_file_is_single_chunk() {
        let data = b"hello harbor";
        let result = chunk_bytes(data);
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunks[0].len, data.len() as u32);
        assert_eq!(result.chunks[0].hash, chunk_hash(data));
    }

    #[test]
    fn boundaries_are_deterministic() {
        let data = pseudo_random(CHUNK_MAX * 3 + 12345, 7);
        let first = chunk_bytes(&data);
        let second = chunk_bytes(&data);
        assert_eq!(first.chunks, second.chunks);
        assert_eq!(first.content_hash, second.content_hash);
        assert!(first.chunks.len() > 2);
    }

    #[test]
    fn chunk_sizes_are_bounded() {
        let data = pseudo_random(CHUNK_MAX * 8 + 999, 42);
        let result = chunk_bytes(&data);
        for chunk in &result.chunks {
            assert!(chunk.len as usize >= CHUNK_MIN, "块过小：{}", chunk.len);
            assert!(chunk.len as usize <= CHUNK_MAX, "块过大：{}", chunk.len);
        }
        let total: u64 = result.chunks.iter().map(|c| c.len as u64).sum();
        assert_eq!(total, data.len() as u64);
        // 偏移必须连续。
        let mut expected = 0;
        for chunk in &result.chunks {
            assert_eq!(chunk.offset, expected);
            expected = chunk.end();
        }
    }

    #[test]
    fn local_change_does_not_shift_all_boundaries() {
        let mut data = pseudo_random(CHUNK_MAX * 6, 99);
        let before = chunk_bytes(&data);
        data.splice(100_000..100_000, [1, 2, 3, 4, 5, 6, 7, 8]);
        let after = chunk_bytes(&data);

        let before_hashes: std::collections::HashSet<[u8; 32]> =
            before.chunks.iter().map(|c| c.hash).collect();
        let reused = after
            .chunks
            .iter()
            .filter(|c| before_hashes.contains(&c.hash))
            .count();
        assert!(
            reused >= after.chunks.len().saturating_sub(2),
            "局部插入后应复用绝大多数块：{reused}/{}",
            after.chunks.len()
        );
    }

    #[test]
    fn file_reader_matches_bytes() {
        let data = pseudo_random(3 * 1024 * 1024 + 77, 5);
        let from_bytes = chunk_bytes(&data);
        let from_reader =
            chunk_reader(std::io::Cursor::new(data.clone()), data.len() as u64).unwrap();
        assert_eq!(from_bytes.chunks, from_reader.chunks);
        assert_eq!(from_bytes.content_hash, from_reader.content_hash);
    }
}
