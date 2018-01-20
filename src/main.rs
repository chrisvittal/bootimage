#![feature(termination_trait)]
#![feature(try_from)]

extern crate byteorder;
extern crate xmas_elf;
extern crate git2;
extern crate structopt;
#[macro_use]
extern crate structopt_derive;
extern crate toml;

use std::io;
use std::io::prelude::*;
use std::fs::File;
use std::path::Path;
use std::process::Command;
use std::convert::TryFrom;
use byteorder::{ByteOrder, LittleEndian};
use git2::Repository;
use structopt::StructOpt;

#[derive(StructOpt, Debug)]
#[structopt(name = "TODO", about = "TODO")]
struct Opt {
    #[structopt(long = "release", help = "Compile kernel in release mode")]
    release: bool,

    #[structopt(short = "o", long = "output", help = "Output file", default_value = "image.bin")]
    output: String,

    #[structopt(long = "target", help = "Target triple")]
    target: Option<String>,
}

fn main() -> io::Result<()> {
    let opt = Opt::from_args();
    let mut pwd = std::env::current_dir()?;

    // try to find Cargo.toml to find root dir and read crate name
    let crate_name = loop {
        let mut cargo_toml_path = pwd.clone();
        cargo_toml_path.push("Cargo.toml");
        match File::open(cargo_toml_path) {
            Err(ref e) if e.kind() == io::ErrorKind::NotFound => {
                if !pwd.pop() {
                    panic!("Cargo.toml not found");
                }
            }
            Err(e) => return Err(e),
            Ok(mut file) => {
                // Cargo.toml found! pwd is already the root path, so we just need
                // to set the crate name
                let mut content = String::new();
                file.read_to_string(&mut content)?;
                let toml = content.parse::<toml::Value>().ok();
                if let Some(toml) = toml {
                    if let Some(crate_name) = toml.get("package")
                        .and_then(|package| package.get("name")).and_then(|n| n.as_str())
                    {
                        break String::from(crate_name);
                    }
                }
                panic!("Cargo.toml invalid");
            }
        }
    };

    let bootloader_target = "test";
    let kernel_target = opt.target.as_ref().map(String::as_str);

    // compile kernel
    let exit_status = run_xargo_build(&pwd, kernel_target, opt.release)?;
    if !exit_status.success() { std::process::exit(1) }

    let mut out_dir = pwd.to_path_buf();
    out_dir.push("target");
    if let Some(target) = kernel_target {
        out_dir.push(target);
    }
    if opt.release {
        out_dir.push("release");
    } else {
        out_dir.push("debug");
    }
    let mut bootloader_dir = out_dir.clone();
    bootloader_dir.push("bootloader");
    if !bootloader_dir.exists() {
        // download bootloader from github repo
        let url = "https://github.com/phil-opp/rustboot-x86";
        println!("Cloning bootloader from {}", url);
        Repository::clone(url, &bootloader_dir).expect("failed to clone bootloader");
    }

    let mut bootloader_path = bootloader_dir.to_path_buf();
    bootloader_path.push("bootloader.bin");
    if !bootloader_path.exists() {
        // compile bootloader
        println!("Compiling bootloader...");
        let exit_status = run_xargo_build(&bootloader_dir, Some(bootloader_target), true)?;
        if !exit_status.success() { std::process::exit(1) }

        let mut bootloader_elf_path = bootloader_dir.to_path_buf();
        bootloader_elf_path.push("target");
        bootloader_elf_path.push(bootloader_target);
        bootloader_elf_path.push("release/elf_loader");

        let mut bootloader_elf_bytes = Vec::new();
        File::open(bootloader_elf_path)?.read_to_end(&mut bootloader_elf_bytes)?;

        // copy bootloader section of ELF file to bootloader_path
        let elf_file = xmas_elf::ElfFile::new(&bootloader_elf_bytes).unwrap();
        xmas_elf::header::sanity_check(&elf_file).unwrap();
        let bootloader_section = elf_file.find_section_by_name(".bootloader")
            .expect("bootloader must have a .bootloader section");

        File::create(&bootloader_path)?.write_all(bootloader_section.raw_data(&elf_file))?;
    }

    let mut output = File::create(opt.output)?;

    let mut kernel_path = out_dir.clone();
    kernel_path.push(crate_name);
    let mut kernel = File::open(kernel_path)?;
    let kernel_size = kernel.metadata()?.len();
    let kernel_size = u32::try_from(kernel_size).expect("kernel too big");
    let mut kernel_interface_block = [0u8; 512];
    LittleEndian::write_u32(&mut kernel_interface_block[0..4], kernel_size);

    let mut bootloader_data = Vec::new();
    File::open(&bootloader_path)?.read_to_end(&mut bootloader_data)?;

    output.write_all(&bootloader_data)?;
    output.write_all(&kernel_interface_block)?;

    // write out kernel elf file
    let mut buffer = [0u8; 1024];
    loop {
        let (n, interrupted) = match kernel.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => (n, false),
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => (0, true),
            Err(e) => return Err(e),
        };
        if !interrupted {
            output.write_all(&buffer[..n])?
        }
    }

    Ok(())
}

fn run_xargo_build(pwd: &Path, target: Option<&str>, release: bool) -> io::Result<std::process::ExitStatus> {
    let mut command = Command::new("xargo");
    command.current_dir(pwd).env("RUST_TARGET_PATH", pwd);
    command.arg("build");
    if let Some(target) = target {
        command.arg("--target").arg(target);
    }
    if release {
        command.arg("--release");
    }
    command.status()
}
