#[cfg(test)]
mod tests {
    use x86_oracle::X86Oracle;

    /// ADD X0, X1, X2
    const A64_ADD_X0_X1_X2: u32 = 0x8B02_0020;
    /// add x3, x1, x2
    const RV_ADD_X3_X1_X2: u32 = 0x0020_81B3;
    /// add rax, rbx
    const X86_ADD_RAX_RBX: [u8; 3] = [0x48, 0x01, 0xD8];

    #[test]
    fn aarch64_riscv_and_x86_in_one_binary() {
        let mut arm = sail_aarch64_oracle::Oracle::new();
        arm.set_x(1, 3);
        arm.set_x(2, 4);
        arm.set_pc(0x1000);
        arm.step(A64_ADD_X0_X1_X2);
        assert_eq!(arm.get_x(0), 7);
        assert_eq!(arm.get_pc(), 0x1004);

        let mut rv = sail_riscv_oracle::Oracle::new();
        rv.set_x(1, 5);
        rv.set_x(2, 7);
        assert!(rv.step(RV_ADD_X3_X1_X2).retired);
        assert_eq!(rv.get_x(3), 12);

        let mut x86 = x86_oracle::SailOracle::new();
        x86.set_rip(x86_oracle::DEFAULT_CODE_ADDR);
        x86.set_gpr(x86_oracle::RAX, 1);
        x86.set_gpr(x86_oracle::RBX, 2);
        assert!(x86.step_bytes(&X86_ADD_RAX_RBX).is_retired());
        assert_eq!(x86.get_gpr(x86_oracle::RAX), 3);

        // Interleave once more: each model must still see only its own runtime state.
        arm.write_mem_u64(0x8000_0000, 0x1122_3344_5566_7788);
        x86.write_mem_u64(0x6_0000, 0x1234);
        assert_eq!(arm.read_mem_u64(0x8000_0000), 0x1122_3344_5566_7788);
        assert!(!arm.is_mapped(0x6_0000));
        assert_eq!(x86.read_mem_u64(0x6_0000), 0x1234);
        assert!(!x86.is_mapped(0x8000_0000));
        arm.step(A64_ADD_X0_X1_X2);
        assert_eq!(arm.get_pc(), 0x1008);
        assert!(rv.step(RV_ADD_X3_X1_X2).retired);
        assert_eq!(rv.get_pc(), sail_riscv_oracle::DEFAULT_RESET_PC + 8);
    }
}
