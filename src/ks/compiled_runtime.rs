use super::*;
use crate::ks::compiled::Program;

#[derive(Clone)]
enum Cell {
    Value(Value),
    Name(String),
    Member(Value, String),
}
impl Cell {
    fn value(&self, runtime: &Runtime<'_>) -> Result<Value, Fault> {
        match self {
            Self::Value(v) => Ok(v.clone()),
            Self::Name(n) => runtime.get(n),
            Self::Member(v, k) => runtime.member(v.clone(), k.clone()),
        }
    }
    fn set(&self, runtime: &mut Runtime<'_>, value: Value, declare: bool) -> Result<(), Fault> {
        match self {
            Self::Name(n) if declare => {
                runtime
                    .scopes
                    .last()
                    .unwrap()
                    .borrow_mut()
                    .insert(n.clone(), value);
                Ok(())
            }
            Self::Name(n) => runtime.set(n, value),
            Self::Member(v, k) => runtime.assign_member(v.clone(), k.clone(), value),
            _ => Err("compiled KS assignment requires a reference".into()),
        }
    }
    fn label(&self) -> String {
        match self {
            Self::Name(n) | Self::Member(_, n) => n.clone(),
            _ => "expression".into(),
        }
    }
}
fn pop(stack: &mut Vec<Cell>) -> Result<Cell, Fault> {
    stack
        .pop()
        .ok_or_else(|| "KS bytecode stack underflow".into())
}
impl Runtime<'_> {
    fn compiled_expression(
        &mut self,
        values: Vec<Value>,
        expression: Expr,
    ) -> Result<Value, Fault> {
        let scope = values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (format!("\0compiled{i}"), v))
            .collect();
        self.scopes.push(Rc::new(RefCell::new(scope)));
        let result = self.eval(&expression);
        self.scopes.pop();
        result
    }
    pub(super) fn compiled(&mut self, program: &Rc<Program>, entry: usize) -> Result<Flow, Fault> {
        let mut stack: Vec<Cell> = Vec::new();
        let mut pc = entry;
        let mut compound = false;
        let mut iterations: HashMap<usize, (Vec<Value>, usize)> = HashMap::new();
        let mut handlers: Vec<(usize, usize, usize)> = Vec::new();
        let arg = |i| Box::new(Expr::Name(format!("\0compiled{i}")));
        loop {
            self.tick()?;
            let i = program
                .instructions
                .get(pc)
                .ok_or("compiled KS fell off program")?;
            pc += 1;
            let result: Result<Option<Flow>, Fault> = (|| {
                let mut push = None;
                match i.op {
                    0 => return Ok(Some(Flow::Next)),
                    1 => self.write(&i.text)?,
                    2 => stack.push(Cell::Name(i.text.clone())),
                    3 => push = Some(Value::Number(i.number as f64)),
                    0x3f => push = Some(Value::Number(i.real)),
                    4..=8 | 0xc..=0x14 | 0x16..=0x18 => {
                        let b = pop(&mut stack)?.value(self)?;
                        let target = pop(&mut stack)?;
                        let a = target.value(self)?;
                        let op = match i.op {
                            4 => "+",
                            5 => "-",
                            6 => "*",
                            7 => "/",
                            8 => "%",
                            0xc => "<",
                            0xd => ">",
                            0xe => "<=",
                            0xf => ">=",
                            0x13 => "==",
                            0x14 => "!=",
                            0x17 => "&",
                            _ => {
                                return Err(
                                    format!("unverified compiled operator {:#x}", i.op).into()
                                )
                            }
                        };
                        let value = self.compiled_expression(
                            vec![a, b],
                            Expr::Binary(op.into(), arg(0), arg(1)),
                        )?;
                        if compound {
                            target.set(self, value.clone(), false)?;
                            compound = false;
                        }
                        push = Some(value);
                    }
                    9 | 0x2c => {
                        let value = pop(&mut stack)?.value(self)?;
                        let target = pop(&mut stack)?;
                        target.set(self, value.clone(), i.op == 0x2c)?;
                        // The declaration separator can consume this reference too.
                        stack.push(target);
                    }
                    0xa => compound = true,
                    0xb => {
                        pop(&mut stack)?;
                    }
                    0x15 => {
                        let value = pop(&mut stack)?.value(self)?;
                        self.write_filtered(&value.text())?;
                    }
                    0x19 | 0x36 => {
                        let value = stack
                            .last()
                            .cloned()
                            .ok_or("compiled KS duplicate on empty stack")?;
                        stack.push(value);
                    }
                    0x1a | 0x1b => {
                        let value = pop(&mut stack)?.value(self)?.truth();
                        if value == (i.op == 0x1b) {
                            pc = program.jump((i.end as i64 + i.number as i64) as usize)?;
                        }
                    }
                    0x1d => push = Some(Value::Bool(!pop(&mut stack)?.value(self)?.truth())),
                    0x1c => {
                        let value = pop(&mut stack)?.value(self)?;
                        push = Some(
                            self.compiled_expression(vec![value], Expr::Unary("-".into(), arg(0)))?,
                        );
                    }
                    0x21 | 0x22 => {
                        let target = pop(&mut stack)?;
                        let old = target.value(self)?;
                        target.set(
                            self,
                            Value::Number(old.number()? + if i.op == 0x21 { 1.0 } else { -1.0 }),
                            false,
                        )?;
                        push = Some(old);
                    }
                    0x23 => pc = program.jump((i.end as i64 + i.number as i64) as usize)?,
                    0x24 | 0x28 => {}
                    0x25 => push = Some(Value::Null),
                    0x26 => push = Some(Value::Bool(i.number != 0)),
                    0x27 => return Ok(Some(Flow::Return(pop(&mut stack)?.value(self)?))),
                    0x2a | 0x30 => {
                        let mut args = Vec::new();
                        for _ in 0..i.number {
                            args.push(pop(&mut stack)?.value(self)?);
                        }
                        args.reverse();
                        let target = pop(&mut stack)?;
                        let label = target.label();
                        let value = if i.op == 0x30 {
                            let name = match &target {
                                Cell::Name(n) => n.clone(),
                                _ => return Err("compiled constructor must be named".into()),
                            };
                            let expressions = (0..args.len()).map(|j| *arg(j)).collect();
                            self.compiled_expression(
                                args,
                                Expr::Unary(
                                    "new".into(),
                                    Box::new(Expr::Call(Box::new(Expr::Name(name)), expressions)),
                                ),
                            )?
                        } else {
                            let receiver = match &target {
                                Cell::Member(v, _) => Some(v.clone()),
                                _ => None,
                            };
                            match target.value(self)? {
                                Value::Function(f) => self.invoke(f, args, receiver, label)?,
                                f => self.call_host(f, args, receiver, label)?,
                            }
                        };
                        stack.push(Cell::Value(value));
                        if i.op == 0x30 {
                            push = Some(Value::Null);
                        }
                    }
                    0x2b => {
                        let b = pop(&mut stack)?;
                        let a = pop(&mut stack)?;
                        stack.push(b);
                        stack.push(a);
                    }
                    0x2d => {
                        let target = pop(&mut stack)?;
                        if let Cell::Name(n) = &target {
                            if !self.scopes.last().unwrap().borrow().contains_key(n) {
                                target.set(self, Value::Null, true)?;
                            }
                        }
                    }
                    0x2e => push = Some(self.get("this")?),
                    0x2f | 0x31 => {
                        let key = pop(&mut stack)?;
                        let key = if i.op == 0x2f {
                            key.label()
                        } else {
                            key.value(self)?.text()
                        };
                        let base = pop(&mut stack)?.value(self)?;
                        stack.push(Cell::Member(base, key));
                    }
                    0x34 => push = Some(Value::String(i.text.clone())),
                    0x39 => return Err(Fault::Script(pop(&mut stack)?.value(self)?)),
                    0x55 => {
                        // Verified anonymous function without parameters. Its
                        // body is skipped here and invoked with the closure.
                        if ![0x29, 0xb, 0xb].into_iter().enumerate().all(|(n, op)| {
                            program.instructions.get(pc + n).is_some_and(|x| x.op == op)
                        }) {
                            return Err("unsupported anonymous compiled function prologue".into());
                        }
                        push = Some(self.function(
                            None,
                            vec![],
                            Rc::new(vec![Stmt::Compiled(program.clone(), pc + 3)]),
                        ));
                        pc = program.jump((i.end as i64 + i.number as i64) as usize)?;
                    }
                    0x3a => {
                        if i.offsets[1] != 0 {
                            return Err("compiled finally requires implementation".into());
                        }
                        let catch = program.jump(i.at + 12 + i.offsets[0] as usize)?;
                        let end = program.jump(i.at + 20 + i.offsets[2] as usize)?;
                        handlers.push((catch, end, stack.len()));
                    }
                    0x3b => {
                        let (_, end, _) = handlers.pop().ok_or("compiled catch without try")?;
                        pc = end;
                    }
                    0x3c => {}
                    0x35 => {
                        let source = pop(&mut stack)?.value(self)?;
                        let target = pop(&mut stack)?;
                        if !iterations.contains_key(&i.at) {
                            let mut keys = match source {
                                Value::List(l) => (0..self.list_ids(&l.borrow())?.len())
                                    .map(|n| Value::Number(n as f64))
                                    .collect::<Vec<_>>(),
                                Value::Array(o) => o
                                    .borrow()
                                    .keys()
                                    .filter(|k| k.as_str() != "length")
                                    .map(|k| {
                                        k.parse::<usize>()
                                            .map(|n| Value::Number(n as f64))
                                            .unwrap_or_else(|_| Value::String(k.clone()))
                                    })
                                    .collect(),
                                Value::Object(o) | Value::ReadOnlyObject(o) => {
                                    Self::keys(&o)?.into_iter().map(Value::String).collect()
                                }
                                _ => {
                                    return Err("compiled for-in requires enumerable object".into())
                                }
                            };
                            keys.sort_by(|a, b| match (a, b) {
                                (Value::Number(a), Value::Number(b)) => a.total_cmp(b),
                                _ => a.text().cmp(&b.text()),
                            });
                            iterations.insert(i.at, (keys, 0));
                        }
                        let (keys, n) = iterations.get_mut(&i.at).unwrap();
                        if let Some(key) = keys.get(*n) {
                            target.set(self, key.clone(), false)?;
                            *n += 1;
                            push = Some(Value::Bool(true));
                        } else {
                            iterations.remove(&i.at);
                            push = Some(Value::Bool(false));
                        }
                    }
                    _ => {
                        return Err(format!("compiled KS opcode {:#x} not implemented", i.op).into())
                    }
                }
                if let Some(value) = push {
                    stack.push(Cell::Value(value));
                }
                Ok(None)
            })();
            match result {
                Ok(Some(flow)) => return Ok(flow),
                Ok(None) => {}
                Err(Fault::Host(e)) => {
                    return Err(Fault::Host(format!(
                        "bytecode {:#x} (opcode {:#x}): {e}",
                        i.at, i.op
                    )))
                }
                Err(Fault::Script(value)) => {
                    if let Some((catch, _, len)) = handlers.pop() {
                        stack.truncate(len);
                        stack.push(Cell::Value(value));
                        pc = catch;
                    } else {
                        return Err(Fault::Script(value));
                    }
                }
            }
        }
    }
}
