use crate::lang::{execution_context::ExecutionContext, job::JobJoinHandle, command::CrushCommand, value::ValueDefinition};
use crate::lang::{argument::ArgumentDefinition, argument::ArgumentVecCompiler, value::Value};
use crate::lang::scope::Scope;
use crate::lang::errors::{error, CrushResult, Kind};
use crate::lang::printer::printer;
use crate::lang::stream::{ValueReceiver, ValueSender};
use crate::util::thread::{handle, build};
use std::path::Path;

#[derive(Clone)]
pub struct CommandInvocation {
    command: ValueDefinition,
    arguments: Vec<ArgumentDefinition>,
}

fn resolve_external_command(name: &str, env: Scope) -> Option<Box<Path>> {
    if let Value::List(path) = env.get("cmd_path")? {
        let path_vec = path.dump();
        for val in path_vec {
            match val {
                Value::File(el) => {
                    let full = el.join(name);
                    if full.exists() {
                        return Some(full.into_boxed_path());
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn arg_can_block(local_arguments: &Vec<ArgumentDefinition>, env: &Scope) -> bool {
    for arg in local_arguments {
        if arg.value.can_block(local_arguments, env) {
            return true;
        }
    }
    false
}

impl CommandInvocation {
    pub fn new(command: ValueDefinition, arguments: Vec<ArgumentDefinition>) -> CommandInvocation {
        CommandInvocation { command, arguments }
    }

    pub fn as_string(&self) -> Option<String> {
        if self.arguments.len() != 0 {
            return None;
        }

        match &self.command {
            ValueDefinition::Value(Value::String(s)) => Some(s.to_string()),
            _ => None
        }
    }

        pub fn arguments(&self) -> &Vec<ArgumentDefinition> {
        &self.arguments
    }

    /*
    pub fn spawn_stream(
        &self,
        env: &Scope,
        mut argument_stream: InputStream,
        output: ValueSender,
    ) -> CrushResult<JobJoinHandle> {
        let cmd = env.get(&self.name);
        match cmd {
            Some(Value::Command(command)) => {
                let c = command.call;
                Ok(handle(build(format_name(&self.name)).spawn(
                    move || {
                        loop {
                            match argument_stream.recv() {
                                Ok(mut row) => {}
                                Err(_) => break,
                            }
                        }
                        Ok(())
                    })))
            }
            _ => {
                error("Can't stream call")
            }
        }
    }
*/
    fn make_context(
        deps: &mut Vec<JobJoinHandle>,
        local_arguments: Vec<ArgumentDefinition>,
        local_env: Scope,
        mut this: Option<Value>,
        input: ValueReceiver,
        output: ValueSender,
    ) -> CrushResult<ExecutionContext> {
        let (arguments, arg_this) = local_arguments
            .compile(deps, &local_env)?;

        if arg_this.is_some() {
            this = arg_this;
        }

        Ok(ExecutionContext {
            input,
            output,
            arguments,
            env: local_env,
            this,
        })
    }

    pub fn can_block(&self, arg: &Vec<ArgumentDefinition>, env: &Scope) -> bool {
        let cmd = self.command.compile_non_blocking(env);
        match cmd {
            Ok((_, Value::Command(command))) =>
                command.can_block(arg, env) || arg_can_block(&self.arguments, env),

            _ => true,
        }
    }

    pub fn invoke(
        &self,
        env: &Scope,
        input: ValueReceiver,
        output: ValueSender) -> CrushResult<JobJoinHandle> {
        match self.command.compile_non_blocking(env) {
            Ok((this, value)) => {
                invoke_value(this, value, self.arguments.clone(), env, input, output)
            }
            Err(err) => {
                if err.kind == Kind::BlockError {
                    let cmd = self.command.clone();
                    let e = env.clone();
                    let arguments = self.arguments.clone();
                    Ok(handle(build(self.command.to_string().as_str()).spawn(
                        move || {
                            let mut dep = Vec::new();
                            match cmd.compile(&mut dep, &e) {
                                Ok((this, value)) => {
                                    printer().handle_error(invoke_value(this, value, arguments, &e, input, output));
                                }

                                Err(err) => {
                                    if let ValueDefinition::Label(p) = &cmd {
                                        printer().handle_error(try_external_command(&p, arguments, &e, input, output));
                                    } else {
                                        printer().handle_error::<()>(Err(err));
                                    }
                                }
                            }
                        })))
                } else {
                    if let ValueDefinition::Label(p) = &self.command {
                        try_external_command(&p, self.arguments.clone(), env, input, output)
                    } else {
                        Err(err)
                    }
                }
            }
        }
    }
}

fn invoke_value(
    this: Option<Value>,
    value: Value,
    local_arguments: Vec<ArgumentDefinition>,
    env: &Scope,
    input: ValueReceiver,
    output: ValueSender) -> CrushResult<JobJoinHandle> {
    let local_env = env.clone();
    match value {
        Value::Command(command) =>
            invoke_command(command, this, local_arguments, local_env, input, output),
        Value::File(f) =>
            if local_arguments.len() == 0 {
                let meta = f.metadata();
                if meta.is_ok() && meta.unwrap().is_dir() {
                    invoke_command(
                        CrushCommand::command_undocumented(crate::lib::traversal::cd, false),
                        None,
                        vec![ArgumentDefinition::unnamed(ValueDefinition::Value(Value::File(f)))],
                        local_env, input, output)
                } else {
                    invoke_command(
                        CrushCommand::command_undocumented(crate::lib::input::val, false),
                        None,
                        vec![ArgumentDefinition::unnamed(ValueDefinition::Value(Value::File(f)))],
                        local_env, input, output)
                }
            } else {
                error(format!("Not a command {}", f.to_str().unwrap_or("<invalid filename>")).as_str())
            }
        Value::Type(t) => {
            match t.fields().get(&Box::from("__call_type__")) {
                None =>
                    invoke_command(
                        CrushCommand::command_undocumented(crate::lib::input::val, false),
                        None,
                        vec![ArgumentDefinition::unnamed(ValueDefinition::Value(Value::Type(t)))],
                        local_env, input, output),
                Some(call) =>
                    invoke_command(
                        call.as_ref().clone(),
                        this,
                        local_arguments,
                        local_env, input, output),
            }
        }
        _ =>
            if local_arguments.len() == 0 {
                invoke_command(
                    CrushCommand::command_undocumented(crate::lib::input::val, false),
                    None,
                    vec![ArgumentDefinition::unnamed(ValueDefinition::Value(value))],
                    local_env, input, output)
            } else {
                error(format!("Not a command {}", value.to_string()).as_str())
            }
    }
}

fn invoke_command(
    action: Box<dyn CrushCommand + Sync + Send>,
    this: Option<Value>,
    local_arguments: Vec<ArgumentDefinition>,
    local_env: Scope,
    input: ValueReceiver,
    output: ValueSender) -> CrushResult<JobJoinHandle> {
    if !action.can_block(&local_arguments, &local_env) && !arg_can_block(&local_arguments, &local_env) {
        let mut deps: Vec<JobJoinHandle> = Vec::new();
        let context = CommandInvocation::make_context(
            &mut deps,
            local_arguments,
            local_env,
            this,
            input, output)?;
        printer().handle_error(action.invoke(context));
        Ok(JobJoinHandle::Many(deps))
    } else {
        Ok(handle(build(action.name()).spawn(
            move || {
                let mut deps: Vec<JobJoinHandle> = Vec::new();
                let context = CommandInvocation::make_context(
                    &mut deps,
                    local_arguments,
                    local_env,
                    this,
                    input, output);
                if let Ok(ctx) = context {
                    printer().handle_error(action.invoke(ctx));
                } else {
                    printer().handle_error(context);
                }
            })))
    }
}

fn try_external_command(p: &str, mut arguments: Vec<ArgumentDefinition>, env: &Scope, input: ValueReceiver,
                        output: ValueSender) -> CrushResult<JobJoinHandle> {
    match resolve_external_command(p, env.clone()) {
        None => error(format!("Unknown command name {}", p).as_str()),
        Some(path) => {
            arguments.insert(
                0,
                ArgumentDefinition::unnamed(ValueDefinition::Value(Value::File(path))));
            let cmd = CommandInvocation {
                command: ValueDefinition::Value(Value::Command(CrushCommand::command_undocumented(crate::lib::control::cmd, true))),
                arguments,
            };
            cmd.invoke(env, input, output)
        }
    }
}

impl ToString for CommandInvocation {
    fn to_string(&self) -> String {
        self.command.to_string()
    }
}
