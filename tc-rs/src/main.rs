#![no_main]
#![no_std]
#![feature(abi_efiapi)]
#![allow(stable_features)]
#![allow(unused_imports)]
#![allow(dead_code)]
#![allow(unused_mut)]
#![allow(unused_variables)]

extern crate alloc;

use alloc::{format, vec};
use core::alloc::Layout;
use core::ffi::{c_void, CStr};
use core::mem::MaybeUninit;

use alloc::string::ToString;
use alloc::vec::Vec;
use binread::io::SeekFrom;
use binread::BinResult;
use core::alloc::GlobalAlloc;
use log::info;
use ntfs::attribute_value::NtfsAttributeValue;
use ntfs::structured_values::NtfsStructuredValue;
use ntfs::{Ntfs, NtfsFile};
use uefi::proto::device_path::{self, text::*, DevicePath};
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::file::FileInfo;
use uefi::table::boot::{BootServices, LoadImageSource, MemoryType, ScopedProtocol};
use uefi::table::runtime::{VariableAttributes, VariableVendor};
use uefi::{guid, CString16, Guid};
use uefi::{
    prelude::*,
    proto::media::{
        block::{self, BlockIO, BlockIOMedia},
        file::{File, FileAttribute, FileMode, FileSystemVolumeLabel},
        fs::SimpleFileSystem,
    },
    table::boot::{OpenProtocolAttributes, OpenProtocolParams},
    CStr16,
};

struct BlockWrapper<'a> {
    bio: ScopedProtocol<'a, BlockIO>,
    cur: u64,
    mid: u32,
    blocksize: usize,
    blockcount: usize,
    bbblock: u64,
    blockbuf: [u8; 4096],
}
impl binread::io::Read for BlockWrapper<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, binread::io::Error> {
        if buf.len() == 0 {
            return Ok(0);
        }

        let mut blockbuf = [0u8; 4096];
        let curblock = self.cur / self.blocksize as u64;
        let curblockmod = self.cur as usize % self.blocksize;

        // only read 1 block for ez
        if curblockmod != 0 {
            // self.bio
            //     .read_blocks(self.mid, curblock, &mut blockbuf[..self.blocksize])
            //     .unwrap();
            let len = core::cmp::min(buf.len(), self.blocksize - curblockmod);
            let buflen = buf.len();
            let blocksz = self.blocksize;
            let cc = self.cur;
            // info!("{cc} {blocksz} {curblockmod} {buflen} {len} {}..{}", curblockmod, len);
            buf.copy_from_slice(&self.blockbuf[curblockmod..curblockmod + len]);
            self.cur += len as u64;
            self.bbblock += 1;
            return Ok(len);
        }

        let blocks = buf.len() / self.blocksize;

        if blocks == 0 {
            let postmod = buf.len() % self.blocksize;
            // self.bio
            //     .read_blocks(self.mid, curblock, &mut blockbuf)
            //     .unwrap();
            buf.copy_from_slice(&self.blockbuf[..postmod]);
            self.cur += postmod as u64;
            Ok(postmod)
        } else {
            let len = blocks * self.blocksize;
            self.bio
                .read_blocks(self.mid, curblock, &mut buf[..len])
                .unwrap();
            self.cur += len as u64;
            // self.blockbuf.copy_from_slice(&buf[(blocks-1) * self.blocksize..len]);
            self.bbblock += blocks as u64;
            self.bio
                .read_blocks(self.mid, self.bbblock, &mut self.blockbuf[..self.blocksize])
                .unwrap();
            Ok(len)
        }
    }
}
impl binread::io::Seek for BlockWrapper<'_> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, binread::io::Error> {
        let x = match pos {
            SeekFrom::Start(p) => {
                self.cur = p;
                Ok(p)
            }
            SeekFrom::End(p) => {
                self.cur = ((self.blocksize * self.blockcount) as i64 + p) as u64;
                Ok(self.cur)
            }
            SeekFrom::Current(p) => {
                self.cur = (self.cur as i64 + p) as u64;
                Ok(self.cur)
            }
        };
        self.bbblock = self.cur / self.blocksize as u64;
        self.bio
            .read_blocks(self.mid, self.bbblock, &mut self.blockbuf[..self.blocksize])
            .unwrap();
        return x;
    }
}

static mut BOOTSYS: Option<SystemTable<Boot>> = None;
static mut CURIMG: Option<Handle> = None;

const EFIGUARDPROTOGUID: Guid = guid!("51e4785b-b1e4-4fda-af5f-942ec015f107");

#[repr(C)]
pub struct EfiGuardProtocol {
    config: extern "efiapi" fn(configdata: *const u8) -> Status,
}

extern "efiapi" fn driver_configure(_: *const u8) -> Status {
    info!("driver_configure called! uninstalling guard protocol...");
    unsafe {
        BOOTSYS
            .take()
            .unwrap()
            .boot_services()
            .uninstall_protocol_interface(
                CURIMG.unwrap(),
                &EFIGUARDPROTOGUID,
                &mut EFIGUARDPROTOINTERFACE as *mut EfiGuardProtocol as *mut c_void,
            )
            .unwrap()
    }
    unsafe {
        BOOTSYS = None;
    }
    Status::SUCCESS
}

static mut EFIGUARDPROTOINTERFACE: EfiGuardProtocol = EfiGuardProtocol {
    config: driver_configure,
};

#[entry]
fn main(current_image: Handle, mut system_table: SystemTable<Boot>) -> Status {
    uefi_services::init(&mut system_table).expect("Failed to uefi_services::init()");
    let bs = system_table.boot_services();
    let rs = system_table.runtime_services();
    // info!("Hello world!");

    let guardproto = unsafe {
        bs.install_protocol_interface(
            Some(current_image),
            &EFIGUARDPROTOGUID,
            &mut EFIGUARDPROTOINTERFACE as *mut EfiGuardProtocol as *mut c_void,
        )
        .expect("Failed to install EfiGuard UEFI protocol")
    };

    let (src, dest) = {
        let mut sfs = bs
            .get_image_file_system(current_image)
            .expect("Failed to get parent SimpleFileSystem");
        let mut sfsdir = sfs
            .open_volume()
            .expect("Failed to open parent SimpleFileSystem volume...");

        let mut read_file = |path: &CStr16, pad: bool| -> Vec<u8> {
            let mut f = sfsdir
                .open(&path, FileMode::Read, FileAttribute::empty())
                .unwrap()
                .into_regular_file()
                .expect("Failed to open file as regular file (we don't want a directory)");
            let info = f
                .get_boxed_info::<FileInfo>()
                .expect("Failed to get file info");

            let mut sz = info.file_size() as usize;
            if pad && (sz % 512) != 0 {
                sz = (sz / 512 + 1) * 512
            }
            let mut data = vec![0; sz];
            f.read(data.as_mut_slice())
                .expect("Failed to read data from file");
            data
        };

        let cfgdata = read_file(&cstr16!("\\EFI\\Boot\\config.txt"), false);
        let cftstr = core::str::from_utf8(&cfgdata).unwrap();
        let mut split = cftstr.split("\n");
        let src =
            CString16::try_from(format!("\\EFI\\Boot\\{}", split.next().unwrap().trim()).as_str())
                .unwrap();
        let dest = split.next().unwrap().trim().to_string();
        // info!("looking to replace '{}' with '{}'", dest, src);

        (
            read_file(CStr16::from_u16_with_nul(src.to_u16_slice_with_nul()).unwrap(), true),
            dest,
        )
    };

    let bio_handles = bs.find_handles::<BlockIO>().unwrap();
    for handle in &bio_handles {
        let mut bio = unsafe {
            bs.open_protocol::<BlockIO>(
                OpenProtocolParams {
                    handle: *handle,
                    agent: current_image,
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
            .expect("Failed to get block I/O protocol")
        };

        if !bio.media().is_logical_partition() {
            continue;
        }

        let mid = bio.media().media_id();
        let blocksize = bio.media().block_size() as usize;
        let blockcount = bio.media().last_block() as usize;
        let mut bw = BlockWrapper {
            bio: bio,
            cur: 0,
            mid: mid,
            blocksize: blocksize,
            blockcount: blockcount,
            bbblock: 0,
            blockbuf: [0u8; 4096],
        };
        bw.bio
            .read_blocks(bw.mid, 0, &mut bw.blockbuf[..bw.blocksize])
            .unwrap();

        if let Ok(mut ntfs) = Ntfs::new(&mut bw) {
            info!(
                "NTFS! Volume name = '{}'",
                ntfs.volume_name(&mut bw).unwrap().unwrap().name()
            );

            let mut root = ntfs.root_directory(&mut bw).unwrap();

            let mut get_file = |dir: NtfsFile, mut target: &str| -> Option<NtfsFile> {
                bs.stall(500_000);
                let index = dir.directory_index(&mut bw).ok()?;
                // I would like to use the index.finder() but it didn't work :L
                let mut iter = index.entries();

                while let Some(entry) = iter.next(&mut bw) {
                    let entry = entry.unwrap();
                    let file_name = entry.key().unwrap().unwrap();
                    // info!("{}", file_name.name());
                    if file_name.name() == target {
                        // info!("found");
                        return entry.to_file(&ntfs, &mut bw).ok();
                    }
                }

                None
            };

            if let Some(x) = dest.split('\\').try_fold(root, get_file) {
                info!("Found '{}'!", dest);
                let pos = u64::from(
                    // holy fucking unwrap...
                    x.data(&mut bw, "")
                        .unwrap()
                        .unwrap()
                        .to_attribute()
                        .unwrap()
                        .value(&mut bw)
                        .unwrap()
                        .data_position()
                        .value()
                        .unwrap(),
                );
                // it's assumed that the pos/lba will be block-aligned...
                // and also that the file is NonResident...
                bw.bio
                    .write_blocks(bw.mid, pos / (bw.blocksize as u64), &src)
                    .unwrap();
                bw.bio.flush_blocks().unwrap();
                info!("overwrote file!");
            }
        } else {
            // info!("not NTFS");
        }
    }

    info!("stalling for 5s");
    bs.stall(5_000_000);

    unsafe {
        BOOTSYS = Some(system_table);
        CURIMG = Some(current_image);
    }

    info!("returning from entry point");
    Status::SUCCESS
}
