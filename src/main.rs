use std::collections::HashMap;
use std::ffi::{CStr, OsStr};
use std::fs;
use std::fs::File;
use std::io;
use std::io::{Read, Seek};
use std::os::unix::fs::FileExt;

struct PacData {
    name: String,
    index: HashMap<String, (u64, usize)>,
}

impl PacData {
    const SIGNATURE: [u8; 4] = [0x50, 0x41, 0x43, 0x20];

    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            index: HashMap::new(),
        }
    }

    fn file(&self) -> String {
        format!("{}.pac", self.name)
    }

    fn read(&self) -> io::Result<Vec<(&String, Vec<u8>)>> {
        let pac = File::open(&self.file())?;

        let mut map = vec![];

        for (name, &(offset, len)) in &self.index {
            let mut data = vec![0; len];

            pac.read_at(&mut data, offset)?;
            map.push((name, data));
        }

        Ok(map)
    }

    fn build(mut self) -> io::Result<Self> {
        let mut pac = File::open(&self.file())?;

        let mut b4 = [0u8; 4];
        let mut b8 = [0u8; 8];
        let mut b32 = [0u8; 32];

        pac.read(&mut b4)?;

        if Self::SIGNATURE.eq(&b4) {
            pac.read_at(&mut b8, 0x08)?;
            pac.seek(io::SeekFrom::Start(0x0804))?;

            for _ in 0..usize::from_le_bytes(b8) {
                pac.read(&mut b32)?;

                let ptr = unsafe { CStr::from_ptr(b32.as_ptr() as *const _) };
                let name = ptr.to_string_lossy().to_string();

                pac.read(&mut b4)?;

                let len = u32::from_le_bytes(b4) as usize;

                pac.read(&mut b4)?;

                let offset = u32::from_le_bytes(b4) as u64;

                self.index.insert(name, (offset, len));
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

    fn save(&self) -> io::Result<()> {
        for pac in &self.data {
            let dir = format!("{}/{}", self.name, pac.name);

            fs::create_dir_all(&dir)?;

            for (name, data) in pac.read()? {
                fs::write(format!("{dir}/{name}"), data)?;
            }
        }

        Ok(())
    }

    fn load(mut self, path: &str) -> io::Result<Self> {
        for path in fs::read_dir(path)?
            .filter_map(io::Result::ok)
            .map(|f| f.path())
            .filter(|f| f.extension() == Some(OsStr::new("pac")))
        {
            if let Some(pac) = path
                .file_stem()
                .map(OsStr::to_string_lossy)
                .map(|s| PacData::new(&s))
            {
                self.data.push(pac.build()?);
            }
        }

        Ok(self)
    }
}

fn main() -> io::Result<()> {
    AssetLoader::new("assets").load(".")?.save()
}
