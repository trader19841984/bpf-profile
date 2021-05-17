//! bpf-profile implementation of the profile struct.

use super::dump::Resolver;
use crate::config::{Address, Map, ProgramCounter, GROUND_ZERO};

type Functions = Map<Address, Function>;

/// Represents the profile.
#[derive(Debug)]
pub struct Profile {
    file: String,
    ground: Call,
    functions: Functions,
    dump: Resolver,
}

use super::{fileutil, Error, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

impl Profile {
    /// Creates the initial instance of profile.
    pub fn new(file: String, dump: Resolver) -> Result<Self> {
        let mut functions = Map::new();
        functions.insert(GROUND_ZERO, Function::ground_zero());
        Ok(Profile {
            file,
            ground: Call::new(GROUND_ZERO),
            functions,
            dump,
        })
    }

    /// Reads the trace and creates the profile data.
    pub fn create(trace_file: PathBuf, dump: Resolver) -> Result<Self> {
        tracing::debug!("Profile.create {:?}", &trace_file);

        let mut prof = Profile::new(
            trace_file
                .to_str()
                .ok_or_else(|| Error::Filename(trace_file.clone()))?
                .to_string(),
            dump,
        )?;

        let reader = BufReader::new(fileutil::open(&trace_file)?);
        parse_trace_file(reader, &mut prof)?;
        dbg!(&prof);
        Ok(prof)
    }

    /// Writes the profile data in the callgrind file format.
    /// See details of the format in the Valgrind documentation.
    pub fn write_callgrind(&self, mut output: impl Write) -> Result<()> {
        writeln!(output, "# callgrind format")?;
        writeln!(output, "version: 1")?;
        writeln!(output, "creator: bpf-profile")?;
        writeln!(output, "events: Instructions")?;
        writeln!(
            output,
            "totals: {}",
            self.functions[&GROUND_ZERO].total_cost()
        )?;
        writeln!(output, "fl={}", self.file)?;
        write_callgrind_functions(&self.functions, output)?;
        Ok(())
    }

    /// Increments the total cost and the cost of current call.
    fn increment_cost(&mut self) {
        tracing::debug!("Profile.increment_cost");
        self.ground.increment_cost(&mut self.functions);
    }

    /// Adds next call to the call stack.
    fn push_call(&mut self, call: Call, first_pc: ProgramCounter) {
        let address = call.address;
        tracing::debug!("Profile.push_call {}", address);
        self.ground.push_call(call);
        #[allow(clippy::map_entry)]
        if !self.functions.contains_key(&address) {
            tracing::debug!("Add function to the registry: {}", address);
            let func = Function::new(address, first_pc, &mut self.dump);
            self.functions.insert(address, func);
        }
    }

    /// Removes finished call from the call stack and adds it to the caller.
    fn pop_call(&mut self) {
        let call = self.ground.pop_call();
        tracing::debug!("Profile.pop_call {}", &call.address);
        if !call.is_ground() {
            let f = self
                .functions
                .get_mut(&call.caller)
                .expect("Caller not found in registry of functions");
            f.add_call(call);
        }
    }
}

use super::trace::Instruction;

/// Parses the trace file line by line building the Profile instance.
pub fn parse_trace_file(mut reader: impl BufRead, prof: &mut Profile) -> Result<()> {
    let mut line = String::with_capacity(512);
    let mut bytes_read = usize::MAX;
    let mut lc = 0_usize;
    let mut ix: Instruction;

    while bytes_read != 0 {
        if line.is_empty() {
            bytes_read = fileutil::read_line(&mut reader, &mut line)?;
            lc += 1;
        }

        let ixr = Instruction::parse(&line);
        if let Err(Error::TraceSkipped) = &ixr {
            //warn!("Skip '{}'", &line.trim());
            line.clear();
            continue;
        }
        ix = ixr?;

        if ix.is_exit() {
            prof.increment_cost();
            prof.pop_call();
            line.clear();
            continue;
        }

        if !ix.is_call() {
            prof.increment_cost();
            line.clear();
            continue;
        }

        // Handle sequences of enclosed calls as well:
        // 604: call 0xcb3fc071
        // 588: call 0x8e0001f9
        // 1024: call 0x8bf38212
        // ...
        while ix.is_call() {
            prof.increment_cost();
            let call = Call::from(&ix, lc)?;
            prof.push_call(call, ix.program_counter());
            // Read next line — the first instruction of the call
            bytes_read = fileutil::read_line(&mut reader, &mut line)?;
            lc += 1;
            ix = Instruction::parse(&line)?;
        }
        // Keep here the last non-call line to process further
    }

    Ok(())
}

/// Represents a function call.
#[derive(Clone, Debug)]
struct Call {
    address: Address,
    caller: Address,
    cost: usize,
    callee: Box<Option<Call>>,
    depth: usize,
}

impl Call {
    /// Creates new call object.
    fn new(address: Address) -> Self {
        Call {
            address,
            caller: Address::default(),
            cost: 0,
            callee: Box::new(None),
            depth: 0,
        }
    }

    /// Creates new call object from a trace instruction (which must be a call).
    fn from(ix: &Instruction, lc: usize) -> Result<Self> {
        let text = ix.text();
        if !ix.is_call() {
            return Err(Error::TraceNotCall(text));
        }
        let mut pair = text.split_whitespace(); // => "call something"
        let _ = pair
            .next()
            .ok_or_else(|| Error::TraceParsing(ix.text(), lc))?;
        let address = pair
            .next()
            .ok_or_else(|| Error::TraceParsing(ix.text(), lc))?;
        Ok(Call::new(hex_str_to_address(address)))
    }

    /// Checks if the call is the root ("ground zero").
    fn is_ground(&self) -> bool {
        self.address == GROUND_ZERO
    }

    /// Increments the cost of this call.
    fn increment_cost(&mut self, functions: &mut Functions) {
        tracing::debug!("Call({}).increment_cost", self.address);
        match *self.callee {
            Some(ref mut callee) => {
                callee.increment_cost(functions);
            }
            None => {
                self.cost += 1;
                let f = functions
                    .get_mut(&self.address)
                    .expect("Call address not found in registry of functions");
                f.increment_cost();
            }
        }
    }

    /// Adds next call to the call stack.
    fn push_call(&mut self, mut call: Call) {
        tracing::debug!(
            "Call({}).push_call {} depth={}",
            self.address,
            call.address,
            self.depth
        );
        self.depth += 1;
        match *self.callee {
            Some(ref mut callee) => {
                callee.push_call(call);
            }
            None => {
                call.caller = self.address;
                let old = std::mem::replace(&mut *self.callee, Some(call));
                assert!(old.is_none());
            }
        }
    }

    /// Removes current call from the call stack.
    fn pop_call(&mut self) -> Call {
        tracing::debug!("Call({}).pop_call depth={}", self.address, self.depth);
        assert!(self.callee.is_some());
        self.depth -= 1;
        let callee = self.callee.as_mut().as_mut().unwrap();
        if callee.callee.is_some() {
            callee.pop_call()
        } else {
            let call = self.callee.take().unwrap();
            self.cost += call.cost;
            call
        }
    }
}

/// Converts a hex number string representation to integer Address.
fn hex_str_to_address(s: &str) -> Address {
    let a = s.trim_start_matches("0x");
    Address::from_str_radix(a, 16).expect("Invalid address")
}

/// Represents a function which will be dumped into a profile.
#[derive(Debug)]
struct Function {
    address: Address,
    name: String,
    pc: ProgramCounter,
    cost: usize,
    calls: Vec<Call>,
}

impl Function {
    /// Creates initial function object which stores total cost of entire program.
    fn ground_zero() -> Self {
        Function {
            address: GROUND_ZERO,
            name: "GROUND_ZERO".into(),
            pc: 0,
            cost: 0,
            calls: Vec::new(),
        }
    }

    /// Creates new function object.
    fn new(address: Address, first_pc: ProgramCounter, dump: &mut Resolver) -> Self {
        assert_ne!(address, GROUND_ZERO);
        Function {
            address,
            name: dump.resolve(address, first_pc),
            pc: first_pc,
            cost: 0,
            calls: Vec::new(),
        }
    }

    /// Increments the immediate cost of the function.
    fn increment_cost(&mut self) {
        tracing::debug!("Function({}).increment_cost", self.address);
        self.cost += 1;
    }

    /// Adds finished enclosed call for this function.
    fn add_call(&mut self, call: Call) {
        tracing::debug!("Function({}).add_call {}", self.address, call.address);
        self.calls.push(call);
    }

    /// Returns inclusive cost of the function and of it's calls.
    fn total_cost(&self) -> usize {
        self.calls.iter().fold(self.cost, |sum, c| sum + c.cost)
    }
}

/// Writes information about calls of functions and their costs.
fn write_callgrind_functions(functions: &Functions, mut output: impl Write) -> Result<()> {
    let mut statistics = Map::new();

    for (a, f) in functions {
        if *a == GROUND_ZERO {
            continue;
        }
        writeln!(output)?;
        writeln!(output, "fn={}", f.name)?;
        writeln!(output, "{} {}", f.pc, f.cost)?;
        statistics.clear();
        for c in &f.calls {
            #[allow(clippy::map_entry)]
            if !statistics.contains_key(&c.address) {
                statistics.insert(c.address, (1, c.cost));
            } else {
                let mut stat = statistics[&c.address];
                stat.0 += 1;
                stat.1 += c.cost;
                statistics.insert(c.address, stat);
            }
        }
        for (a, s) in &statistics {
            writeln!(output, "cfn={}", functions[a].name)?;
            writeln!(output, "calls={} {}", s.0, functions[a].pc)?;
            writeln!(output, "{} {}", f.pc, s.1)?;
        }
    }

    output.flush()?;
    Ok(())
}
