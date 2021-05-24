//! bpf-profile dump module.

use super::{buf, Error, Result};
use crate::config::{Address, Index, Map, ProgramCounter, GROUND_ZERO};
use lazy_static::lazy_static;
use regex::Regex;
use std::io::BufRead;
use std::path::Path;

/// Reads the dump file (if any) and returns a dump representation.
pub fn read(filename: Option<&Path>) -> Result<Resolver> {
    match filename {
        None => Ok(Resolver::default()),
        Some(filename) => Resolver::read(filename),
    }
}

/// Represents the dump file contents.
#[derive(Default, Debug)]
pub struct Resolver {
    not_default: bool,
    functions: Vec<String>,
    index_function_by_address: Map<Address, Index>,
    index_function_by_first_pc: Map<ProgramCounter, Index>,
    unresolved_counter: usize,
}

const PREFIX_OF_UNRESOLVED: &str = "function_";

impl Resolver {
    /// Reads the dump file to collect function names.
    /// Returns non-trivial (with real function names) instance of the Resolver.
    fn read(filename: &Path) -> Result<Self> {
        let mut resolver = Resolver::default();
        let reader = buf::open(filename)?;
        parse_dump_file(reader, &mut resolver)?;
        resolver.not_default = true;
        Ok(resolver)
    }

    /// Checks if resolver was generated from nothing (default) or from the dump file.
    pub fn is_default(&self) -> bool {
        !self.not_default
    }

    /// Takes an address and returns name of corresponding function.
    pub fn resolve_by_address(&self, address: Address) -> String {
        tracing::debug!("Resolver.resolve(0x{:x})", &address);
        assert_ne!(address, GROUND_ZERO);
        let func_index = self.index_function_by_address[&address];
        let func_name = self.functions[func_index].clone();
        tracing::debug!("Resolver.resolve returns {})", &func_name);
        func_name
    }

    /// Takes a program counter and returns name of function which begins with it (if any).
    pub fn resolve_by_first_pc(&self, pc: ProgramCounter) -> Option<String> {
        let func_index = self.index_function_by_first_pc.get(&pc);
        func_index.map(|i| self.functions[*i].clone())
    }

    /// Takes an address and returns name of corresponding function,
    /// otherwise returns a generated string if can not resolve properly.
    pub fn update(&mut self, address: Address, first_pc: ProgramCounter) -> String {
        tracing::debug!("Resolver.update(0x{:x}, {})", &address, &first_pc);
        assert_ne!(address, GROUND_ZERO);

        let found = self.index_function_by_address.contains_key(&address);
        if !found {
            if self.contains_function_with_first_pc(first_pc) {
                // There can be multiple copies of one function with different addresses
                let func_index = self.index_function_by_first_pc[&first_pc];
                self.index_function_by_address.insert(address, func_index);
            } else {
                let unresolved_func_name = format!(
                    "{}{} (0x{:x})",
                    PREFIX_OF_UNRESOLVED, self.unresolved_counter, address
                );
                self.unresolved_counter += 1;
                let func_index = self.update_first_pc_index(&unresolved_func_name, first_pc);
                self.index_function_by_address.insert(address, func_index);
            }
        }

        let func_index = self.index_function_by_address[&address];
        let func_name = self.functions[func_index].clone();
        tracing::debug!("Resolver.update returns {})", &func_name);
        func_name
    }

    /// Checks if a function has been indexed already.
    fn contains_function_with_first_pc(&self, first_pc: ProgramCounter) -> bool {
        self.index_function_by_first_pc.contains_key(&first_pc)
    }

    /// Creates new entry in the index of functions by their first instruction's pc.
    fn update_first_pc_index(&mut self, name: &str, first_pc: ProgramCounter) -> Index {
        let func_index = self.functions.len();
        self.functions.push(name.into());
        self.index_function_by_first_pc.insert(first_pc, func_index);
        func_index
    }
}

const HEADER: &str = "ELF Header";
const DISASM_HEADER: &str = "Disassembly of section .text";

/// Parses the dump file building the Resolver instance.
fn parse_dump_file(mut reader: impl BufRead, resolv: &mut Resolver) -> Result<()> {
    let mut line = String::with_capacity(512);
    let mut bytes_read = usize::MAX;
    let mut lc = 0_usize;

    // Skip to the disassembly
    let mut was_header = false;
    let mut was_disasm = false;
    while bytes_read != 0 {
        bytes_read = buf::read_line(&mut reader, &mut line)?;
        lc += 1;
        if line.starts_with(HEADER) {
            was_header = true;
            continue;
        }
        if line.starts_with(DISASM_HEADER) {
            if !was_header {
                return Err(Error::DumpFormat);
            }
            was_disasm = true;
            break;
        }
    }
    if !was_disasm {
        return Err(Error::DumpFormatNoDisasm);
    }

    lazy_static! {
        static ref FUNC_HEADER: Regex =
            Regex::new(r"[[:xdigit:]]+\s+<(.+)>").expect("Invalid regex");
        static ref FUNC_INSTRUCTION: Regex =
            Regex::new(r"\s+(\d+)(\s+[[:xdigit:]]{2}){8}\s+.+").expect("Invalid regex");
    }

    // Read functions and their instructions
    while bytes_read != 0 {
        bytes_read = buf::read_line(&mut reader, &mut line)?;
        lc += 1;
        if let Some(caps) = FUNC_HEADER.captures(&line) {
            let name = caps[1].to_string();
            if !name.starts_with("LBB") {
                // Get the very first instruction of the function
                bytes_read = buf::read_line(&mut reader, &mut line)?;
                lc += 1;
                if let Some(caps) = FUNC_INSTRUCTION.captures(&line) {
                    let pc = caps[1]
                        .parse::<ProgramCounter>()
                        .expect("Cannot parse program counter");
                    if !resolv.contains_function_with_first_pc(pc) {
                        resolv.update_first_pc_index(&name, pc);
                    }
                } else {
                    return Err(Error::DumpParsing(line, lc));
                }
            }
        }
    }

    Ok(())
}