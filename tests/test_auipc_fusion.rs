use std::fs;

use ckb_vm::{
    decoder::{build_decoder, Decoder},
    instructions::{
        execute, extract_opcode, generate_handle_function_list, generate_vcheck_function_list,
        instruction_length, set_instruction_length_n, Instruction, Utype,
    },
    machine::VERSION1,
    CoreMachine, DefaultCoreMachine, DefaultMachine, DefaultMachineBuilder, Error, Memory,
    SparseMemory, SupportMachine, ISA_IMC,
};
use ckb_vm_definitions::instructions as insts;

#[cfg(has_asm)]
use ckb_vm::{
    instructions::{blank_instruction, is_basic_block_end_instruction},
    machine::asm::{AsmCoreMachine, AsmGlueMachine, AsmMachine},
};
#[cfg(has_asm)]
use ckb_vm_definitions::asm::{calculate_slot, Trace, TRACE_ITEM_LENGTH};

// This is simplified from https://github.com/xxuejie/ckb-vm-contrib/blob/main/src/decoder.rs
pub struct AuxDecoder {
    inner: Decoder,
}

impl AuxDecoder {
    pub fn new(inner: Decoder) -> Self {
        Self { inner }
    }

    pub fn decode<M: Memory>(&mut self, memory: &mut M, pc: u64) -> Result<Instruction, Error> {
        let head_inst = self.inner.decode(memory, pc)?;
        match extract_opcode(head_inst) {
            insts::OP_AUIPC => {
                let i = Utype(head_inst);
                let head_len = instruction_length(head_inst);
                let value = pc.wrapping_add(i64::from(i.immediate_s()) as u64);
                if let Ok(value) = value.try_into() {
                    return Ok(set_instruction_length_n(
                        Utype::new(insts::OP_CUSTOM_LOAD_UIMM, i.rd(), value).0,
                        head_len,
                    ));
                }
            }
            _ => (),
        };

        Ok(head_inst)
    }
}

#[test]
pub fn test_rust_auipc_fusion() {
    let buffer = fs::read("tests/programs/auipc_no_sign_extend")
        .unwrap()
        .into();

    let core_machine =
        DefaultCoreMachine::<u64, SparseMemory<u64>>::new(ISA_IMC, VERSION1, u64::max_value());
    let mut machine = DefaultMachineBuilder::new(core_machine).build();
    machine
        .load_program(&buffer, &vec!["auipc_no_sign_extend".into()])
        .unwrap();

    let mut decoder = AuxDecoder::new(build_decoder::<u64>(machine.isa(), machine.version()));
    machine.set_running(true);
    let vcheck_function_list = generate_vcheck_function_list::<
        DefaultMachine<DefaultCoreMachine<u64, SparseMemory<u64>>>,
    >();
    let handle_function_list = generate_handle_function_list::<
        DefaultMachine<DefaultCoreMachine<u64, SparseMemory<u64>>>,
    >();
    while machine.running() {
        let pc = *machine.pc();
        let i = decoder.decode(machine.memory_mut(), pc).expect("decode");

        execute(
            &mut machine,
            &vcheck_function_list,
            &handle_function_list,
            i,
        )
        .expect("execute");
    }

    let result = machine.exit_code();
    assert_eq!(result, 0);
}

#[cfg(has_asm)]
#[test]
pub fn test_asm_auipc_fusion() {
    extern "C" {
        fn ckb_vm_asm_labels();
    }

    let buffer = fs::read("tests/programs/auipc_no_sign_extend")
        .unwrap()
        .into();

    let asm_core = AsmCoreMachine::new(ISA_IMC, VERSION1, u64::max_value());
    let asm_glue = AsmGlueMachine::new(asm_core);
    let core = DefaultMachineBuilder::new(asm_glue).build();
    let mut machine = AsmMachine::new(core, None);
    machine
        .load_program(&buffer, &vec!["auipc_no_sign_extend".into()])
        .unwrap();

    let mut decoder = AuxDecoder::new(build_decoder::<u64>(
        machine.machine.isa(),
        machine.machine.version(),
    ));

    let pc = *machine.machine.pc();
    let slot = calculate_slot(pc);
    let mut trace = Trace::default();
    let mut current_pc = pc;
    let mut i = 0;
    while i < TRACE_ITEM_LENGTH {
        let instruction = decoder
            .decode(machine.machine.memory_mut(), current_pc)
            .unwrap();
        let end_instruction = is_basic_block_end_instruction(instruction);
        current_pc += u64::from(instruction_length(instruction));
        trace.instructions[i] = instruction;
        trace.cycles += machine.machine.instruction_cycle_func()(
            instruction,
            machine.machine.coprocessor_v().vl(),
            machine.machine.coprocessor_v().vsew(),
        );
        let opcode = extract_opcode(instruction);
        // Here we are calculating the absolute address used in direct threading
        // from label offsets.
        trace.thread[i] = unsafe {
            u64::from(*(ckb_vm_asm_labels as *const u32).offset(opcode as u8 as isize))
                + (ckb_vm_asm_labels as *const u32 as u64)
        };
        i += 1;
        if end_instruction {
            break;
        }
    }
    assert_eq!(i, 6);
    assert_eq!(current_pc, 0x1008e);
    trace.instructions[i] = blank_instruction(insts::OP_CUSTOM_TRACE_END);
    trace.thread[i] = unsafe {
        u64::from(*(ckb_vm_asm_labels as *const u32).offset(insts::OP_CUSTOM_TRACE_END as isize))
            + (ckb_vm_asm_labels as *const u32 as u64)
    };
    trace.address = pc;
    trace.length = (current_pc - pc) as u8;
    machine.machine.inner_mut().imc.traces[slot] = trace;

    let result = machine.run().expect("run");
    assert_eq!(result, 0);
}