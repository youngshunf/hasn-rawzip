use rawzip::{Error, ZipArchiveWriter, ZipLocator};

fn empty_zip() -> Vec<u8> {
    ZipArchiveWriter::new(Vec::new()).finish().unwrap()
}

fn slice_result(bytes: &[u8]) -> Result<(), Error> {
    ZipLocator::strict()
        .locate_in_slice(bytes)
        .map(|_| ())
        .map_err(|(_, error)| error)
}

fn reader_result(bytes: &[u8]) -> Result<(), Error> {
    let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    ZipLocator::strict()
        .locate_in_reader(bytes, &mut buffer, bytes.len() as u64)
        .map(|_| ())
        .map_err(|(_, error)| error)
}

fn one_zip() -> Vec<u8> {
    use std::io::Write;
    let mut writer = ZipArchiveWriter::new(Vec::new());
    let (entry, config) = writer.new_file("payload.txt").start().unwrap();
    let mut data = config.wrap(entry);
    data.write_all(b"payload").unwrap();
    let (entry, descriptor) = data.finish().unwrap();
    entry.finish(descriptor).unwrap();
    writer.finish().unwrap()
}

fn full_slice(bytes: &[u8]) -> Result<(), Error> {
    let archive = ZipLocator::strict()
        .locate_in_slice(bytes)
        .map_err(|(_, e)| e)?;
    let mut entries = archive.entries();
    while let Some(entry) = entries.next_entry()? {
        archive.get_entry(entry.wayfinder())?;
    }
    Ok(())
}

fn full_reader(bytes: &[u8]) -> Result<(), Error> {
    let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    let archive = ZipLocator::strict()
        .locate_in_reader(bytes, &mut buffer, bytes.len() as u64)
        .map_err(|(_, e)| e)?;
    let mut entries = archive.entries(&mut buffer);
    while let Some(entry) = entries.next_entry()? {
        archive.get_entry(entry.wayfinder())?;
    }
    Ok(())
}

fn consume<R: std::io::Read>(mut reader: R) -> std::io::Result<u64> {
    let mut total = 0_u64;
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(total);
        }
        total += count as u64;
        if total > 1024 * 1024 {
            return Err(std::io::Error::other("测试内容预算超限"));
        }
    }
}

#[test]
fn strict去掉后真实结构反例仍会被通用政策接受() {
    let mut bytes = empty_zip();
    bytes[4..6].copy_from_slice(&1_u16.to_le_bytes());
    assert!(ZipLocator::new().locate_in_slice(bytes.as_slice()).is_ok());
    assert!(slice_result(&bytes).is_err());
    let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
    assert!(
        ZipLocator::new()
            .locate_in_reader(bytes.as_slice(), &mut buffer, bytes.len() as u64)
            .is_ok()
    );
    assert!(reader_result(&bytes).is_err());
}

#[test]
fn 严格目录尾部错误即使计数已够也不得吞并() {
    let original = one_zip();
    let archive = rawzip::ZipArchive::from_slice(&original).unwrap();
    let eocd = archive.eocd_offset() as usize;
    let mut bytes = original;
    bytes.insert(eocd, 0);
    let shifted = eocd + 1;
    let size = u32::from_le_bytes(bytes[shifted + 12..shifted + 16].try_into().unwrap());
    bytes[shifted + 12..shifted + 16].copy_from_slice(&(size + 1).to_le_bytes());
    assert!(full_slice(&bytes).is_err());
    assert!(full_reader(&bytes).is_err());
}

#[test]
fn 官方13个原字节zip严格遍历且实际解压通过crc与size() {
    let files = [
        "compact.zip",
        "empty-only.zip",
        "manifest-archive-extra.zip",
        "manifest-archive-missing.zip",
        "manifest-device.zip",
        "manifest-directory.zip",
        "manifest-duplicate.zip",
        "manifest-file-digest-mismatch.zip",
        "manifest-file-size-mismatch.zip",
        "manifest-raw-mismatch.zip",
        "manifest-symlink.zip",
        "raw-nofinal.zip",
        "valid.zip",
    ];
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/release-contract");
    for name in files {
        let bytes = std::fs::read(root.join(name)).unwrap();
        let archive = ZipLocator::strict()
            .locate_in_slice(bytes.as_slice())
            .unwrap();
        assert_eq!(archive.end_offset(), bytes.len() as u64);
        let mut entries = archive.entries();
        let mut count = 0;
        let mut duplicates = std::collections::BTreeSet::new();
        let mut repeated = 0;
        while let Some(header) = entries.next_entry().unwrap() {
            count += 1;
            if !duplicates.insert(header.file_path().as_ref().to_vec()) {
                repeated += 1;
            }
            let entry = archive.get_entry(header.wayfinder()).unwrap();
            let actual = match header.compression_method() {
                rawzip::CompressionMethod::DEFLATE => {
                    consume(entry.verifying_reader(flate2::read::DeflateDecoder::new(entry.data())))
                        .unwrap()
                }
                rawzip::CompressionMethod::STORE => {
                    consume(entry.verifying_reader(entry.data())).unwrap()
                }
                method => panic!("官方样本压缩方法不符：{method:?}"),
            };
            assert_eq!(actual, header.uncompressed_size_hint(), "{name}");
        }
        assert_eq!(count, archive.entries_hint());
        if name == "manifest-duplicate.zip" {
            assert_eq!((count, repeated), (5, 2));
        }
        let mut buffer = vec![0; rawzip::RECOMMENDED_BUFFER_SIZE];
        let reader = ZipLocator::strict()
            .locate_in_file(std::fs::File::open(root.join(name)).unwrap(), &mut buffer)
            .unwrap();
        let mut entries = reader.entries(&mut buffer);
        let mut actual_count = 0;
        while let Some(header) = entries.next_entry().unwrap() {
            let entry = reader.get_entry(header.wayfinder()).unwrap();
            let actual = match header.compression_method() {
                rawzip::CompressionMethod::DEFLATE => consume(
                    entry.verifying_reader(flate2::read::DeflateDecoder::new(entry.reader())),
                )
                .unwrap(),
                rawzip::CompressionMethod::STORE => {
                    consume(entry.verifying_reader(entry.reader())).unwrap()
                }
                method => panic!("官方样本压缩方法不符：{method:?}"),
            };
            assert_eq!(actual, header.uncompressed_size_hint(), "reader {name}");
            actual_count += 1;
        }
        assert_eq!(actual_count, count);
    }
}

#[test]
fn 严格local必须拒绝flags方法和crcsize各字段冲突() {
    let original = include_bytes!("../../assets/crc32-not-streamed.zip");
    let archive = rawzip::ZipArchive::from_slice(original).unwrap();
    let header = archive.entries().next_entry().unwrap().unwrap();
    let offset = header.local_header_offset() as usize;
    for field in [6, 8, 14, 18, 22] {
        let mut bytes = original.to_vec();
        bytes[offset + field] ^= 1;
        assert!(full_slice(&bytes).is_err(), "slice local字段{field}");
        assert!(full_reader(&bytes).is_err(), "reader local字段{field}");
    }
}

#[test]
fn 严格巨大zip64声明必须确定拒绝而不解压补成功() {
    let original = include_bytes!("../../assets/zip64.zip");
    let archive = rawzip::ZipArchive::from_slice(original).unwrap();
    let cd = archive.directory_offset() as usize;
    let name_len = u16::from_le_bytes(original[cd + 28..cd + 30].try_into().unwrap()) as usize;
    let extra = cd + 46 + name_len;
    for field in [extra + 4, extra + 12] {
        let mut bytes = original.to_vec();
        bytes[field..field + 8].copy_from_slice(&(u64::MAX - 1).to_le_bytes());
        assert!(full_slice(&bytes).is_err());
        assert!(full_reader(&bytes).is_err());
    }
}

#[test]
fn 严格descriptor不得跨入中央目录借字节补齐() {
    let original = one_zip();
    let archive = rawzip::ZipArchive::from_slice(&original).unwrap();
    let header = archive.entries().next_entry().unwrap().unwrap();
    let cd = archive.directory_offset() as usize;
    let eocd = archive.eocd_offset() as usize;
    let entry = archive.get_entry(header.wayfinder()).unwrap();
    let (_, body_end) = entry.compressed_data_range();
    let mut bytes = original.clone();
    bytes.drain(body_end as usize..cd);
    let shifted_cd = body_end as usize;
    let shifted_eocd = eocd - (cd - shifted_cd);
    bytes[shifted_eocd + 16..shifted_eocd + 20].copy_from_slice(&(shifted_cd as u32).to_le_bytes());
    assert!(full_slice(&bytes).is_err());
    assert!(full_reader(&bytes).is_err());
}

#[test]
fn 严格dd模式不得忽略local非零冲突值() {
    let mut bytes = one_zip();
    bytes[14..18].copy_from_slice(&1_u32.to_le_bytes());
    assert!(full_slice(&bytes).is_err());
    assert!(full_reader(&bytes).is_err());
}

#[test]
fn 严格实际读取必须拒绝内容crc损坏() {
    let mut bytes = one_zip();
    let archive = rawzip::ZipArchive::from_slice(&bytes).unwrap();
    let header = archive.entries().next_entry().unwrap().unwrap();
    let entry = archive.get_entry(header.wayfinder()).unwrap();
    let (start, _) = entry.compressed_data_range();
    bytes[start as usize] ^= 1;
    let archive = ZipLocator::strict()
        .locate_in_slice(bytes.as_slice())
        .unwrap();
    let header = archive.entries().next_entry().unwrap().unwrap();
    let entry = archive.get_entry(header.wayfinder()).unwrap();
    assert!(consume(entry.verifying_reader(entry.data())).is_err());
}

#[test]
fn 严格完整遍历拒绝已声明前缀() {
    let mut bytes = vec![0; 10];
    let mut writer = ZipArchiveWriter::builder()
        .with_offset(10)
        .build(&mut bytes);
    let (entry, config) = writer.new_file("payload.txt").start().unwrap();
    let (entry, descriptor) = config.wrap(entry).finish().unwrap();
    entry.finish(descriptor).unwrap();
    writer.finish().unwrap();
    assert!(full_slice(&bytes).is_err());
    assert!(full_reader(&bytes).is_err());
}

#[test]
fn 严格local拒绝重复zip64块且不忽略截断extra() {
    use rawzip::{Header, extra_fields::ExtraFieldId};
    let mut writer = ZipArchiveWriter::new(Vec::new());
    let (entry, config) = writer
        .new_file("payload.txt")
        .extra_field(ExtraFieldId::ZIP64, &[0; 16], Header::LOCAL)
        .unwrap()
        .extra_field(ExtraFieldId::ZIP64, &[0; 16], Header::LOCAL)
        .unwrap()
        .start()
        .unwrap();
    let (entry, descriptor) = config.wrap(entry).finish().unwrap();
    entry.finish(descriptor).unwrap();
    let bytes = writer.finish().unwrap();
    assert!(full_slice(&bytes).is_err());
    assert!(full_reader(&bytes).is_err());
}

#[test]
fn 严格zip64拒绝重复块截断值和剩余sentinel() {
    let original = include_bytes!("../../assets/zip64.zip");
    let archive = rawzip::ZipArchive::from_slice(original).unwrap();
    let cd = archive.directory_offset() as usize;
    let eocd = archive.eocd_offset() as usize;
    let locator = eocd - 20;
    let record =
        u64::from_le_bytes(original[locator + 8..locator + 16].try_into().unwrap()) as usize;
    let name_len = u16::from_le_bytes(original[cd + 28..cd + 30].try_into().unwrap()) as usize;
    let extra_len = u16::from_le_bytes(original[cd + 30..cd + 32].try_into().unwrap()) as usize;
    let start = cd + 46 + name_len;
    let extra = &original[start..start + extra_len];
    let mut duplicate = original.to_vec();
    duplicate.splice(start + extra_len..start + extra_len, extra.iter().copied());
    duplicate[cd + 30..cd + 32].copy_from_slice(&((extra_len * 2) as u16).to_le_bytes());
    let shifted_record = record + extra_len;
    let shifted_locator = locator + extra_len;
    let shifted_eocd = eocd + extra_len;
    let cd_size = u64::from_le_bytes(original[record + 40..record + 48].try_into().unwrap())
        + extra_len as u64;
    duplicate[shifted_record + 40..shifted_record + 48].copy_from_slice(&cd_size.to_le_bytes());
    duplicate[shifted_locator + 8..shifted_locator + 16]
        .copy_from_slice(&(shifted_record as u64).to_le_bytes());
    let classic = u32::from_le_bytes(original[eocd + 12..eocd + 16].try_into().unwrap());
    if classic != u32::MAX {
        duplicate[shifted_eocd + 12..shifted_eocd + 16]
            .copy_from_slice(&(classic + extra_len as u32).to_le_bytes());
    }
    assert!(full_slice(&duplicate).is_err());
    assert!(full_reader(&duplicate).is_err());
    let mut truncated = original.to_vec();
    truncated[start + 2..start + 4].copy_from_slice(&1_u16.to_le_bytes());
    assert!(full_slice(&truncated).is_err());
    assert!(full_reader(&truncated).is_err());
    let mut sentinel = original.to_vec();
    sentinel[start + 4..start + 12].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(full_slice(&sentinel).is_err());
    assert!(full_reader(&sentinel).is_err());
}

#[test]
fn 严格getentry拒绝local名称与实际descriptor冲突() {
    let original = one_zip();
    let archive = rawzip::ZipArchive::from_slice(&original).unwrap();
    let first = archive.entries().next_entry().unwrap().unwrap();
    let local = archive.get_entry(first.wayfinder()).unwrap();
    let (_, end) = local.compressed_data_range();
    let mut name = original.clone();
    name[30] ^= 1;
    let mut descriptor = original.clone();
    descriptor[end as usize + 4] ^= 1;
    for bytes in [name, descriptor] {
        assert!(full_slice(&bytes).is_err());
        assert!(full_reader(&bytes).is_err());
    }
}

#[test]
fn 严格entry拒绝非零起始盘和缺失zip64必需值() {
    let original = one_zip();
    let archive = rawzip::ZipArchive::from_slice(&original).unwrap();
    let cd = archive.directory_offset() as usize;
    for (offset, value) in [(cd + 34, 1_u32), (cd + 20, u32::MAX), (cd + 42, u32::MAX)] {
        let mut bytes = original.clone();
        if offset == cd + 34 {
            bytes[offset..offset + 2].copy_from_slice(&(value as u16).to_le_bytes());
        } else {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        assert!(full_slice(&bytes).is_err(), "slice字段{offset}");
        assert!(full_reader(&bytes).is_err(), "reader字段{offset}");
    }
}

#[test]
fn 严格zip64拒绝缺locator和多盘声明() {
    let mut missing = empty_zip();
    missing[8..12].fill(0xff);
    assert!(slice_result(&missing).is_err());
    assert!(reader_result(&missing).is_err());
    let original = include_bytes!("../../assets/zip64.zip");
    assert!(full_slice(original).is_ok());
    assert!(full_reader(original).is_ok());
    let eocd = rawzip::ZipArchive::from_slice(original)
        .unwrap()
        .eocd_offset() as usize;
    let locator = eocd - 20;
    let record =
        u64::from_le_bytes(original[locator + 8..locator + 16].try_into().unwrap()) as usize;
    for (offset, value) in [
        (locator + 4, 1_u32),
        (locator + 16, 2),
        (record + 16, 1),
        (record + 20, 1),
    ] {
        let mut bytes = original.to_vec();
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        assert!(slice_result(&bytes).is_err(), "ZIP64 slice字段{offset}");
        assert!(reader_result(&bytes).is_err(), "ZIP64 reader字段{offset}");
    }
}

#[test]
fn 严格目录结束必须与声明条目数完全一致() {
    let original = one_zip();
    let eocd = rawzip::ZipArchive::from_slice(&original)
        .unwrap()
        .eocd_offset() as usize;
    for count in [0_u16, 2] {
        let mut bytes = original.clone();
        bytes[eocd + 8..eocd + 10].copy_from_slice(&count.to_le_bytes());
        bytes[eocd + 10..eocd + 12].copy_from_slice(&count.to_le_bytes());
        assert!(full_slice(&bytes).is_err());
        assert!(full_reader(&bytes).is_err());
    }
    assert!(full_slice(&original).is_ok());
    assert!(full_reader(&original).is_ok());
}

#[test]
fn 严格locator拒绝前后缀与目录大小漂移() {
    let original = empty_zip();
    let mut trailing = original.clone();
    trailing.push(0);
    let mut prefixed = vec![0];
    prefixed.extend_from_slice(&original);
    let mut size = original.clone();
    size[12..16].copy_from_slice(&1_u32.to_le_bytes());
    for bytes in [trailing, prefixed, size] {
        assert!(slice_result(&bytes).is_err());
        assert!(reader_result(&bytes).is_err());
    }
}

#[test]
fn 严格locator拒绝多盘与同盘记录数不一致() {
    let original = empty_zip();
    assert!(slice_result(&original).is_ok());
    assert!(reader_result(&original).is_ok());
    for (offset, value) in [(4, 1_u16), (6, 1), (8, 1)] {
        let mut bytes = original.clone();
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        assert!(
            slice_result(&bytes).is_err(),
            "slice不得接受EOCD字段{offset}"
        );
        assert!(
            reader_result(&bytes).is_err(),
            "reader不得接受EOCD字段{offset}"
        );
    }
}
