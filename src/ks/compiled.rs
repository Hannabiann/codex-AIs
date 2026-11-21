//! Decoder for compiled KS records. Templates stay in the mounted database.
use super::parser::Stmt;
use std::{collections::HashMap, rc::Rc};

#[derive(Clone, Debug)]
pub struct Instruction {
    pub at: usize,
    pub end: usize,
    pub op: u16,
    pub text: String,
    pub number: i32,
    pub offsets: Vec<u32>,
}
#[derive(Clone, Debug)]
pub struct Program {
    pub instructions: Vec<Instruction>,
    pub positions: HashMap<usize, usize>,
}
impl Program {
    pub fn jump(&self, offset: usize) -> Result<usize, String> {
        self.positions
            .get(&offset)
            .copied()
            .ok_or_else(|| format!("KS bytecode jump outside instruction boundary: {offset:#x}"))
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Reader<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or("KS bytecode offset overflow")?;
        let data = self
            .bytes
            .get(self.pos..end)
            .ok_or("truncated KS bytecode")?;
        self.pos = end;
        Ok(data)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn text(&mut self, n: usize) -> Result<String, String> {
        Ok(crate::db::decode(self.bytes(n)?))
    }
}
pub fn parse(bytes: &[u8]) -> Result<Vec<Stmt>, String> {
    let mut r = Reader { bytes, pos: 0 };
    let mut instructions = Vec::new();
    let mut positions = HashMap::new();
    while r.pos < bytes.len() {
        let at = r.pos;
        r.u32()?;
        r.u16()?; // source span retained by the original compiler
        let op = r.u16()?;
        let mut i = Instruction {
            at,
            end: 0,
            op,
            text: String::new(),
            number: 0,
            offsets: vec![],
        };
        match op {
            1 | 0x34 => {
                let n = r.u16()? as usize;
                i.text = r.text(n)?;
            }
            2 | 0x28 => {
                let n = r.u8()? as usize;
                i.text = r.text(n)?;
                if op == 0x28 {
                    i.number = r.u32()? as i32;
                }
            }
            3 | 0x1a | 0x1b | 0x23 => i.number = r.u32()? as i32,
            0x2a | 0x30 => i.number = r.u8()? as i32,
            0x26 => i.number = r.u16()? as i32,
            0x3a => {
                for _ in 0..3 {
                    i.offsets.push(r.u32()?);
                }
            }
            0
            | 4..=0x19
            | 0x1c..=0x22
            | 0x24..=0x25
            | 0x27
            | 0x29
            | 0x2b..=0x2f
            | 0x31
            | 0x35..=0x38
            | 0x3b..=0x3c => {}
            _ => return Err(format!("unsupported KS bytecode opcode {op:#x} at {at:#x}")),
        }
        i.end = r.pos;
        positions.insert(at, instructions.len());
        instructions.push(i);
    }
    let program = Rc::new(Program {
        instructions,
        positions,
    });
    for i in &program.instructions {
        if matches!(i.op, 0x1a | 0x1b | 0x23) {
            let target = (i.end as i64)
                .checked_add(i.number as i64)
                .filter(|n| *n >= 0)
                .ok_or("invalid KS branch")?;
            program.jump(target as usize)?;
        }
    }
    let mut ast = Vec::new();
    for i in &program.instructions {
        if i.op != 0x28 {
            continue;
        }
        let mut p = program.jump(i.number as u32 as usize)?;
        let code = &program.instructions;
        let require = |p: usize, op| -> Result<(), String> {
            if code.get(p).is_some_and(|i| i.op == op) {
                Ok(())
            } else {
                Err(format!(
                    "unsupported compiled function prologue {} at {p}",
                    i.text
                ))
            }
        };
        for op in [0x29, 2, 0x2b, 0x38, 0xb] {
            require(p, op)?;
            p += 1;
        }
        let mut params = Vec::new();
        while code.get(p).is_some_and(|i| i.op == 0x36) {
            for (j, op) in [0x36, 3, 0x31, 2, 0x2b, 0x2c, 0x2d].into_iter().enumerate() {
                require(p + j, op)?;
            }
            if code[p + 1].number as usize != params.len() {
                return Err("nonsequential compiled KS parameters".into());
            }
            params.push(code[p + 3].text.clone());
            p += 7;
        }
        require(p, 0xb)?;
        ast.push(Stmt::Function(
            i.text.clone(),
            params,
            Rc::new(vec![Stmt::Compiled(program.clone(), p + 1)]),
        ));
    }
    let mut main = program
        .instructions
        .iter()
        .take_while(|i| i.op == 0x28)
        .count();
    if program.instructions.get(main).is_some_and(|i| i.op == 0xb) {
        main += 1;
    }
    ast.push(Stmt::Compiled(program, main));
    Ok(ast)
}
