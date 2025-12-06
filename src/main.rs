#![feature(ptr_as_ref_unchecked)]

use std::collections::HashMap;
use std::ffi::{CStr, OsStr};
use std::fs;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::BitXor;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::ptr;
use std::rc::Rc;

use image::{ImageBuffer, Pixel, Rgba};

struct GeImage;

impl GeImage {
    const MAIN_MAGIC: [u8; 4] = [0x47, 0x45, 0x20, 0x00];
    const SUB_MAGIC: [u8; 4] = [0x50, 0x47, 0x44, 0x33];

    fn decompress(input: &[u8], size_orig: usize) -> Vec<u8> {
        let mut output = vec![0; size_orig];
        let mut output_pos = 0;
        let mut input_pos = 0;
        let mut control = 0;
        while output_pos < output.len() {
            control >>= 1;
            if 0 == control & 0x0100 {
                control = input[input_pos] as u16 | 0xff00;
                input_pos += 1;
            }
            if 0 == control & 1 {
                let mut repetitions = input[input_pos];
                input_pos += 1;
                while output_pos < output.len() && repetitions > 0 {
                    output[output_pos] = input[input_pos];
                    output_pos += 1;
                    input_pos += 1;
                    repetitions -= 1;
                }
            } else {
                let mut tmp = u16::from_le_bytes([input[input_pos], input[input_pos + 1]]) as u32;
                input_pos += 2;
                let (mut repetitions, look_behind) = if 0 == tmp & 8 {
                    tmp = tmp << 8 | input[input_pos] as u32;
                    input_pos += 1;
                    (((tmp & 0x0ffc) >> 2) + 1 << 2 | tmp & 3, tmp >> 12)
                } else {
                    ((tmp & 7) + 4, tmp >> 4)
                };
                let mut pos = output_pos - look_behind as usize;
                while output_pos < output.len() && repetitions > 0 {
                    output[output_pos] = output[pos];
                    output_pos += 1;
                    pos += 1;
                    repetitions -= 1;
                }
            }
        }
        output
    }

    fn apply_filter(data: &[u8], width: usize, height: usize) -> Vec<u8> {
        let stride = width * 3;
        let size = width * height;
        let _data = unsafe {
            (ptr::slice_from_raw_parts(data.as_ptr(), data.len()) as *const [i8]).as_ref_unchecked()
        };
        let mut plane1 = 0;
        let mut plane2 = size >> 2;
        let mut plane3 = size >> 1;
        let mut output = vec![0; height * stride];
        let mut output_pos = 0;
        for _ in 0..height >> 1 {
            for _ in 0..width >> 1 {
                let b = 226 * _data[plane1] as i32;
                let g = -43 * _data[plane1] as i32 - 89 * _data[plane2] as i32;
                let r = 179 * _data[plane2] as i32;
                for i in [0, 1, width, width + 1] {
                    let base = (data[plane3 + i] as i32) << 7;
                    output[output_pos + 3 * i] = (base + b >> 7).clamp(0, 255) as u8;
                    output[output_pos + 3 * i + 1] = (base + g >> 7).clamp(0, 255) as u8;
                    output[output_pos + 3 * i + 2] = (base + r >> 7).clamp(0, 255) as u8;
                }
                plane1 += 1;
                plane2 += 1;
                plane3 += 2;
                output_pos += 6;
            }
            plane3 += width;
            output_pos += stride;
        }
        output
    }

    fn apply_delta_filter(
        data: &mut [u8],
        deltas: &[u8],
        width: usize,
        height: usize,
        channels: usize,
    ) {
        let stride = width * channels;
        for y in 0..height {
            unsafe {
                let prev = data.as_ptr().add((y - 1) * stride);
                let next = data.as_ptr().add(y * stride) as *mut u8;
                match deltas[y] {
                    1 => {
                        for x in channels..stride {
                            *next.add(x) = *next.add(x - channels) - *next.add(x);
                        }
                    }
                    2 => {
                        for x in 0..stride {
                            *next.add(x) = *prev.add(x) - *next.add(x);
                        }
                    }
                    4 => {
                        for x in channels..stride {
                            let mean = (*prev.add(x) as u16 + *next.add(x - channels) as u16) >> 1;
                            *next.add(x) = mean as u8 - *next.add(x);
                        }
                    }
                    _ => todo!(),
                }
            }
        }
    }

    fn decode_main(file: &mut File) -> anyhow::Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        let mut b2 = [0; 2];
        let mut b4 = [0; 4];
        file.seek(SeekFrom::Current(8))?;
        file.read(&mut b4)?;
        let width = u32::from_le_bytes(b4) as usize;
        file.read(&mut b4)?;
        let height = u32::from_le_bytes(b4) as usize;
        file.seek(SeekFrom::Current(8))?;
        file.read(&mut b2)?;
        let filter_type = u16::from_le_bytes(b2);
        file.seek(SeekFrom::Current(2))?;
        file.read(&mut b4)?;
        let size_orig = u32::from_le_bytes(b4) as usize;
        file.read(&mut b4)?;
        let size_comp = u32::from_le_bytes(b4) as usize;
        let mut data = vec![0; size_comp];
        file.read(&mut data)?;
        let mut data = GeImage::decompress(&data, size_orig);
        Ok(match filter_type {
            2 => {
                let data = GeImage::apply_filter(&data, width, height);
                let mut pos = 0;
                ImageBuffer::from_fn(width as u32, height as u32, |_, _| {
                    let b = data[pos];
                    pos += 1;
                    let g = data[pos];
                    pos += 1;
                    let r = data[pos];
                    pos += 1;
                    Rgba([r, g, b, 0xff])
                })
            }
            3 => {
                let channels = u16::from_le_bytes([data[2], data[3]]) as usize >> 3;
                let (_data, data) = data.split_at_mut(8 + height);
                GeImage::apply_delta_filter(data, &_data[8..], width, height, channels);
                let mut pos = 0;
                ImageBuffer::from_fn(width as u32, height as u32, |_, _| {
                    let b = data[pos];
                    pos += 1;
                    let g = data[pos];
                    pos += 1;
                    let r = data[pos];
                    pos += 1;
                    let mut a = 0xff;
                    if channels == 4 {
                        a = data[pos];
                        pos += 1;
                    }
                    Rgba([r, g, b, a])
                })
            }
            _ => todo!(),
        })
    }

    fn decode_sub(
        file: &mut File,
        images: &HashMap<Rc<String>, ImageBuffer<Rgba<u8>, Vec<u8>>>,
    ) -> anyhow::Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        let mut b2 = [0; 2];
        let mut b4 = [0; 4];
        let mut b32 = [0; 32];
        file.read(&mut b2)?;
        let x = u16::from_le_bytes(b2) as u32;
        file.read(&mut b2)?;
        let y = u16::from_le_bytes(b2) as u32;
        file.read(&mut b2)?;
        let width = u16::from_le_bytes(b2) as usize;
        file.read(&mut b2)?;
        let height = u16::from_le_bytes(b2) as usize;
        file.read(&mut b2)?;
        let channels = u16::from_le_bytes(b2) as usize >> 3;
        file.read(&mut b32)?;
        file.read(&mut b2)?;
        file.read(&mut b4)?;
        let size_orig = u32::from_le_bytes(b4) as usize;
        file.read(&mut b4)?;
        let size_comp = u32::from_le_bytes(b4) as usize;
        let mut data = vec![0; size_comp];
        file.read(&mut data)?;
        let mut data = GeImage::decompress(&data, size_orig);
        let (deltas, data) = data.split_at_mut(height);
        GeImage::apply_delta_filter(data, deltas, width, height, channels);
        let mut image = images[&CStr::from_bytes_until_nul(&b32)?
            .to_string_lossy()
            .to_lowercase()]
            .clone();
        let mut pos = 0;
        for _y in 0..height as u32 {
            for _x in 0..width as u32 {
                let b = data[pos];
                pos += 1;
                let g = data[pos];
                pos += 1;
                let r = data[pos];
                pos += 1;
                let mut a = 0;
                if channels == 4 {
                    a = data[pos];
                    pos += 1;
                }
                image
                    .get_pixel_mut(_x + x, _y + y)
                    .apply2(&Rgba([r, g, b, a]), BitXor::bitxor);
            }
        }
        Ok(image)
    }
}

struct PacData {
    name: String,
    metadata: HashMap<Rc<String>, (u64, usize)>,
}

impl PacData {
    const MAGIC: [u8; 4] = [0x50, 0x41, 0x43, 0x20];

    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            metadata: HashMap::new(),
        }
    }

    fn build(mut self) -> anyhow::Result<Self> {
        let mut pac = File::open(&self.name)?;
        let mut b4 = [0; 4];
        let mut b8 = [0; 8];
        let mut b32 = [0; 32];
        pac.read(&mut b4)?;
        if Self::MAGIC.eq(&b4) {
            pac.read_at(&mut b8, 8)?;
            pac.seek(SeekFrom::Start(0x0804))?;
            for _ in 0..usize::from_le_bytes(b8) {
                pac.read(&mut b32)?;
                let ptr = unsafe { CStr::from_ptr(b32.as_ptr() as *const _) };
                let name = ptr.to_string_lossy().to_lowercase();
                pac.read(&mut b4)?;
                let len = u32::from_le_bytes(b4) as usize;
                pac.read(&mut b4)?;
                let offset = u32::from_le_bytes(b4) as u64;
                self.metadata.insert(Rc::new(name), (offset, len));
            }
        }
        Ok(self)
    }
}

struct AssetLoader {
    name: String,
    data: Vec<PacData>,
}

impl AssetLoader {
    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            data: vec![],
        }
    }

    fn save(&mut self) -> anyhow::Result<()> {
        for pac in &mut self.data {
            let dir = Path::new(&self.name).join(&pac.name);
            fs::create_dir_all(&dir)?;
            let mut file = File::open(&pac.name)?;
            let mut main_images = HashMap::new();
            let mut sub_images = vec![];
            for (name, &(offset, len)) in &pac.metadata {
                if name.ends_with("pgd") {
                    let mut magic = [0; 4];
                    file.seek(SeekFrom::Start(offset))?;
                    file.read(&mut magic)?;
                    match magic {
                        GeImage::MAIN_MAGIC => {
                            let image = GeImage::decode_main(&mut file)?;
                            let mut path = dir.join(name.as_ref());
                            path.set_extension("png");
                            image.save(&path)?;
                            main_images.insert(name.clone(), image);
                            println!("FINISHED: {path:?}");
                        }
                        GeImage::SUB_MAGIC => {
                            sub_images.push((name.clone(), offset + 4));
                        }
                        _ => todo!(),
                    };
                } else {
                    let path = dir.join(name.as_ref());
                    let mut data = vec![0; len];
                    file.read_at(&mut data, offset)?;
                    fs::write(&path, data)?;
                    println!("FINISHED: {path:?}");
                }
            }
            for (name, offset) in sub_images {
                let mut path = dir.join(name.as_ref());
                file.seek(SeekFrom::Start(offset))?;
                path.set_extension("png");
                GeImage::decode_sub(&mut file, &main_images)?.save(&path)?;
                println!("FINISHED: {path:?}");
            }
        }
        Ok(())
    }

    fn load(mut self, path: &str) -> anyhow::Result<Self> {
        for pac in fs::read_dir(path)?
            .filter_map(|f| f.ok())
            .map(|f| f.path())
            .filter(|p| p.extension() == Some(OsStr::new("pac")))
            .map(|p| PacData::new(&p.to_string_lossy()))
        {
            self.data.push(pac.build()?);
        }
        Ok(self)
    }
}

fn main() -> anyhow::Result<()> {
    AssetLoader::new("assets").load(".")?.save()
}
