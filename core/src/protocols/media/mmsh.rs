use super::*;
use tokio::io::AsyncSeekExt;

impl Transfer {
    pub(super) async fn mmsh(&self, url: Url) -> Result<(), String> {
        let response = self.get(&url, None, true).await?;
        let mut reader = BodyReader {
            response,
            pending: Vec::new(),
            offset: 0,
            transfer: self,
        };
        let mut header = Vec::new();
        let mut header_location = 0u32;
        let mut properties = None;
        let mut packets = 0u64;
        let mut previous_location = None;
        let mut previous_flags: Option<u8> = None;
        let mut file = tokio::fs::File::create(self.state.join("media.part"))
            .await
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        self.task
            .lock()
            .unwrap()
            .append_log("Native MS-WMSP HTTP Play and ASF packet decapsulation");
        loop {
            let framing = reader
                .take(4)
                .await?
                .ok_or("MMSH ended without an end-of-stream packet")?;
            if framing[0] & 0x7f != b'$' {
                return Err("invalid MMSH framing marker".into());
            }
            let length = u16::from_le_bytes([framing[2], framing[3]]) as usize;
            let body = reader.take(length).await?.ok_or("truncated MMSH packet")?;
            match framing[1] {
                b'H' | b'D' | b'M' => {
                    if body.len() < 8
                        || u16::from_le_bytes([body[6], body[7]]) as usize != body.len()
                    {
                        return Err("invalid MMSH data packet size".into());
                    }
                    let location = u32::from_le_bytes(body[..4].try_into().unwrap());
                    let flags = body[5];
                    let payload = &body[8..];
                    if framing[1] == b'M' {
                        continue;
                    }
                    if framing[1] == b'H' {
                        if properties.is_some() || packets > 0 || location != header_location {
                            return Err("unexpected or out-of-order MMSH ASF header".into());
                        }
                        header_location = header_location
                            .checked_add(1)
                            .ok_or("MMSH header sequence overflow")?;
                        if header.len().saturating_add(payload.len()) > DOCUMENT_LIMIT {
                            return Err("MMSH ASF header exceeds 4 MiB".into());
                        }
                        header.extend_from_slice(payload);
                        if flags & 8 != 0 || flags == 0 {
                            properties = Some(asf_header(&header)?);
                            file.write_all(&header).await.map_err(|e| e.to_string())?;
                            written = header.len() as u64;
                            self.progress(written);
                        }
                    } else {
                        let (_, _, packet_size) =
                            properties.ok_or("MMSH data arrived before a complete ASF header")?;
                        if previous_location
                            .is_some_and(|prior: u32| prior.checked_add(1) != Some(location))
                            || previous_flags.is_some_and(|prior| prior.wrapping_add(1) != flags)
                        {
                            return Err("MMSH skipped or reordered an ASF data packet".into());
                        }
                        previous_location = Some(location);
                        previous_flags = Some(flags);
                        let packet = asf_packet(payload, packet_size)?;
                        file.write_all(&packet).await.map_err(|e| e.to_string())?;
                        packets += 1;
                        written += packet.len() as u64;
                        self.progress(written);
                    }
                }
                b'E' => {
                    if body.len() != 4 {
                        return Err("invalid MMSH end packet length".into());
                    }
                    let reason = u32::from_le_bytes(body[..4].try_into().unwrap());
                    if reason == 1 {
                        return Err(
                            "MMSH server-side multi-entry playlists are not supported".into()
                        );
                    }
                    if reason != 0 {
                        return Err(format!("MMSH server ended with HRESULT 0x{reason:08x}"));
                    }
                    break;
                }
                b'C' => {
                    return Err(
                        "MMSH stream/header changes are not supported in one ASF output".into(),
                    );
                }
                _ => return Err("unsupported MMSH packet type".into()),
            }
        }
        if packets == 0 {
            return Err("MMSH server sent no ASF media packets".into());
        }
        let (header_size, file_properties, _) = properties.ok_or("MMSH has no ASF header")?;
        // Rewrite unknown streaming lengths to match the completed recording.
        header[file_properties + 40..file_properties + 48].copy_from_slice(&written.to_le_bytes());
        header[file_properties + 56..file_properties + 64].copy_from_slice(&packets.to_le_bytes());
        let flags = u32::from_le_bytes(
            header[file_properties + 88..file_properties + 92]
                .try_into()
                .unwrap(),
        ) & !3;
        header[file_properties + 88..file_properties + 92].copy_from_slice(&flags.to_le_bytes());
        header[header_size + 16..header_size + 24]
            .copy_from_slice(&(written - header_size as u64).to_le_bytes());
        header[header_size + 40..header_size + 48].copy_from_slice(&packets.to_le_bytes());
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|e| e.to_string())?;
        file.write_all(&header).await.map_err(|e| e.to_string())?;
        self.finish(file, "asf").await
    }

}

struct BodyReader<'a> {
    response: Response,
    pending: Vec<u8>,
    offset: usize,
    transfer: &'a Transfer,
}
impl BodyReader<'_> {
    async fn take(&mut self, length: usize) -> Result<Option<Vec<u8>>, String> {
        while self.pending.len() - self.offset < length {
            if self.offset > 0 {
                self.pending.drain(..self.offset);
                self.offset = 0;
            }
            let Some(chunk) = self
                .response
                .chunk()
                .await
                .map_err(|e| format!("MMSH body: {e}"))?
            else {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Err("truncated MMSH packet".into())
                };
            };
            self.transfer.limit(chunk.len()).await;
            self.pending.extend_from_slice(&chunk);
        }
        let bytes = self.pending[self.offset..self.offset + length].to_vec();
        self.offset += length;
        Ok(Some(bytes))
    }
}

pub(super) const ASF_HEADER: [u8; 16] = [
    0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];
pub(super) const ASF_DATA: [u8; 16] = [
    0x36, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];
pub(super) const ASF_FILE_PROPERTIES: [u8; 16] = [
    0xa1, 0xdc, 0xab, 0x8c, 0x47, 0xa9, 0xcf, 0x11, 0x8e, 0xe4, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];
pub(super) const ASF_STREAM_PROPERTIES: [u8; 16] = [
    0x91, 0x07, 0xdc, 0xb7, 0xb7, 0xa9, 0xcf, 0x11, 0x8e, 0xe6, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];

pub(super) fn asf_header(bytes: &[u8]) -> Result<(usize, usize, usize), String> {
    if bytes.len() < 80 || !bytes.starts_with(&ASF_HEADER) {
        return Err("MMSH header is not ASF".into());
    }
    let length = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().unwrap()))
        .map_err(|_| "ASF header size overflow")?;
    if length < 30
        || length.checked_add(50) != Some(bytes.len())
        || bytes[length..length + 16] != ASF_DATA
    {
        return Err("invalid ASF Header/Data Object lengths".into());
    }
    for guid in [
        "fbb3112223bdd211b4b700a0c955fc6e",
        "14e68a292226174cb935dae07ee9289c",
        "338505435f32964aaa67a6b29b3b06b4",
    ] {
        let guid = hex::decode(guid).unwrap();
        if bytes[..length].windows(16).any(|window| window == guid) {
            return Err("DRM-protected ASF is not supported".into());
        }
    }
    let count = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let mut position = 30usize;
    let mut properties = None;
    let mut stream = false;
    for _ in 0..count {
        if position + 24 > length {
            return Err("truncated ASF subobject".into());
        }
        let size = usize::try_from(u64::from_le_bytes(
            bytes[position + 16..position + 24].try_into().unwrap(),
        ))
        .map_err(|_| "ASF object size overflow")?;
        let end = position
            .checked_add(size)
            .filter(|end| *end <= length && size >= 24)
            .ok_or("invalid ASF subobject size")?;
        if bytes[position..position + 16] == ASF_FILE_PROPERTIES {
            if size < 104 || properties.is_some() {
                return Err("invalid ASF File Properties Object".into());
            }
            let minimum =
                u32::from_le_bytes(bytes[position + 92..position + 96].try_into().unwrap());
            let maximum =
                u32::from_le_bytes(bytes[position + 96..position + 100].try_into().unwrap());
            if minimum < 16 || minimum != maximum || maximum > (1 << 20) {
                return Err("MMSH supports fixed-size ASF packets up to 1 MiB".into());
            }
            properties = Some((position, maximum as usize));
        }
        if bytes[position..position + 16] == ASF_STREAM_PROPERTIES {
            if size < 78 {
                return Err("invalid ASF Stream Properties Object".into());
            }
            if bytes[position + 73] & 0x80 != 0 {
                return Err("encrypted ASF stream is not supported".into());
            }
            stream = true;
        }
        position = end;
    }
    if position != length || !stream {
        return Err("ASF header has invalid objects or no stream".into());
    }
    let (properties, size) = properties.ok_or("ASF header has no File Properties Object")?;
    Ok((length, properties, size))
}

/// Restore padding stripped by MMSH. Packet Length may be implicit because the
/// accepted ASF header specifies equal minimum/maximum packet sizes.
pub(super) fn asf_packet(bytes: &[u8], packet_size: usize) -> Result<Vec<u8>, String> {
    if bytes.len() > packet_size || bytes.len() < 8 {
        return Err("invalid ASF packet length".into());
    }
    let offset = if bytes[0] & 0x80 != 0 {
        1 + (bytes[0] & 0x0f) as usize
    } else {
        0
    };
    if offset + 2 > bytes.len() {
        return Err("truncated ASF packet header".into());
    }
    let flags = bytes[offset];
    let sizes = [0usize, 1, 2, 4];
    let length_size = sizes[((flags >> 5) & 3) as usize];
    let sequence_size = sizes[((flags >> 1) & 3) as usize];
    let padding_size = sizes[((flags >> 3) & 3) as usize];
    let padding_offset = offset + 2 + length_size + sequence_size;
    let payload_offset = padding_offset + padding_size;
    if payload_offset + 6 > bytes.len() {
        return Err("truncated ASF packet parsing information".into());
    }
    let mut existing_padding = [0u8; 4];
    existing_padding[..padding_size].copy_from_slice(&bytes[padding_offset..payload_offset]);
    let existing_padding = u32::from_le_bytes(existing_padding) as usize;
    if existing_padding > bytes.len() - payload_offset - 6 {
        return Err("invalid ASF packet padding length".into());
    }
    if bytes.len() == packet_size {
        return Ok(bytes.to_vec());
    }
    let new_length = bytes.len() - length_size - padding_size + 4;
    if new_length > packet_size {
        return Err("ASF padding cannot be restored within its fixed packet size".into());
    }
    let mut packet = Vec::with_capacity(packet_size);
    packet.extend_from_slice(&bytes[..offset]);
    packet.push((flags & !0x78) | 0x18); // implicit size; DWORD padding length
    packet.push(bytes[offset + 1]);
    packet.extend_from_slice(&bytes[offset + 2 + length_size..padding_offset]);
    packet.extend_from_slice(&((existing_padding + packet_size - new_length) as u32).to_le_bytes());
    packet.extend_from_slice(&bytes[payload_offset..]);
    packet.resize(packet_size, 0);
    Ok(packet)
}
