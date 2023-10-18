use std::fmt::{Debug, Display};
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;
use std::thread;
#[cfg(debug_assertions)]
use no_deadlocks::Mutex;
#[cfg(not(debug_assertions))]
use std::sync::Mutex;

use async_trait::async_trait;
use indexmap::IndexMap;

use crate::{Attribute, ParsingError, TopElement, Types, ProcessManager, Syntax, TopElementManager, is_modifier, Modifier, ParsingFuture, DataType, SimpleVariableManager};
use crate::async_util::{AsyncDataGetter, NameResolver};
use crate::code::{Expression, FinalizedEffects, FinalizedExpression, FinalizedMemberField, MemberField};
use crate::types::FinalizedTypes;

/// The static data of a function, which is set during parsing and immutable throughout the entire compilation process.
/// Generics will copy this and change the name and types, but never modify the original.
#[derive(Clone, Debug)]
pub struct FunctionData {
    pub attributes: Vec<Attribute>,
    pub modifiers: u8,
    pub name: String,
    pub poisoned: Vec<ParsingError>,
}

impl FunctionData {
    pub fn new(attributes: Vec<Attribute>, modifiers: u8, name: String) -> Self {
        return Self {
            attributes,
            modifiers,
            name,
            poisoned: Vec::new(),
        };
    }

    /// Creates an empty function data that errored while parsing.
    pub fn poisoned(name: String, error: ParsingError) -> Self {
        return Self {
            attributes: Vec::new(),
            modifiers: 0,
            name,
            poisoned: vec!(error),
        };
    }
}

/// Allows generic access to FunctionData.
#[async_trait]
impl TopElement for FunctionData {
    type Unfinalized = UnfinalizedFunction;
    type Finalized = CodelessFinalizedFunction;

    fn set_id(&mut self, _id: u64) {
        //Ignored. Funcs don't have IDs
    }

    fn poison(&mut self, error: ParsingError) {
        self.poisoned.push(error);
    }

    fn is_operator(&self) -> bool {
        return false;
    }

    fn is_trait(&self) -> bool {
        return is_modifier(self.modifiers, Modifier::Trait);
    }

    fn errors(&self) -> &Vec<ParsingError> {
        return &self.poisoned;
    }

    fn name(&self) -> &String {
        return &self.name;
    }

    fn new_poisoned(name: String, error: ParsingError) -> Self {
        return FunctionData::poisoned(name, error);
    }

    /// Verifies the function and adds it to the compiler after it finished verifying.
    async fn verify(current: UnfinalizedFunction, syntax: Arc<Mutex<Syntax>>,
                    resolver: Box<dyn NameResolver>, process_manager: Box<dyn ProcessManager>) {
        let name = current.data.name.clone();
        // Get the codeless finalized function and the code from the function.
        let (codeless_function, code) = process_manager.verify_func(current, &syntax).await;
        // Finalize the code and combine it with the codeless finalized function.
        let finalized_function = process_manager.verify_code(codeless_function, code, resolver, &syntax).await;
        // Add the finalized code to the compiling list.
        syntax.lock().unwrap().compiling.write().unwrap().insert(name, Arc::new(finalized_function));
    }

    fn get_manager(syntax: &mut Syntax) -> &mut TopElementManager<Self> {
        return &mut syntax.functions;
    }
}

/// An unfinalized function is the unlinked function directly after parsing, with no code.
/// Code is finalizied separately and combined with this to make a FinalizedFunction.
pub struct UnfinalizedFunction {
    pub generics: IndexMap<String, Vec<ParsingFuture<Types>>>,
    pub fields: Vec<ParsingFuture<MemberField>>,
    pub code: CodeBody,
    pub return_type: Option<ParsingFuture<Types>>,
    pub data: Arc<FunctionData>,
}

/// Gives generic access to the function data.
impl DataType<FunctionData> for UnfinalizedFunction {
    fn data(&self) -> &Arc<FunctionData> {
        return &self.data;
    }
}

/// If the code is required to finalize the function, then recursive function calls will deadlock.
/// That's why this codeless variant exists, which allows the function data to be finalized before the code itself.
/// This is combined with the FinalizedCodeBody into a FinalizedFunction which is passed to the compiler.
/// (see add_code below)
#[derive(Clone, Debug)]
pub struct CodelessFinalizedFunction {
    pub generics: IndexMap<String, Vec<FinalizedTypes>>,
    pub arguments: Vec<FinalizedMemberField>,
    pub return_type: Option<FinalizedTypes>,
    pub data: Arc<FunctionData>,
}

impl CodelessFinalizedFunction {
    /// Combines the CodelessFinalizedFunction with a FinalizedCodeBody to get a FinalizedFunction.
    pub fn add_code(self, code: FinalizedCodeBody) -> FinalizedFunction {
        return FinalizedFunction {
            generics: self.generics,
            fields: self.arguments,
            code,
            return_type: self.return_type,
            data: self.data,
        };
    }

    /// Makes a copy of the CodelessFinalizedFunction with all the generics solidified into their actual type.
    /// Figures out the solidified types by comparing generics against the input effect types,
    /// then replaces all generic types with their solidified types.
    /// This can't always figure out return types, so an optional return type variable is passed as well
    /// for function calls that include them (see Effects::MethodCall)
    /// The VariableManager here is for the arguments to the function, and not for the function itself.
    pub async fn degeneric(method: Arc<CodelessFinalizedFunction>, mut manager: Box<dyn ProcessManager>,
                           arguments: &Vec<FinalizedEffects>, syntax: &Arc<Mutex<Syntax>>,
                           variables: &SimpleVariableManager,
                           returning: Option<FinalizedTypes>) -> Result<Arc<CodelessFinalizedFunction>, ParsingError> {
        // Degenerics the return type if there is one and returning is some.
        if let Some(inner) = method.return_type.clone() {
            if let Some(mut returning) = returning {
                if let FinalizedTypes::GenericType(inner, _) = returning {
                    returning = FinalizedTypes::clone(inner.deref());
                }

                if let Some((old, other)) =
                    inner.resolve_generic(&returning, syntax,
                                          placeholder_error("Invalid bounds!".to_string())).await? {
                    if let FinalizedTypes::Generic(name, _) = old {
                        manager.mut_generics().insert(name, other);
                    } else {
                        panic!("resolve_generic should never return any type other than the generic to replace!");
                    }
                }
            }
        }

        //Degenerics the arguments to the method
        for i in 0..method.arguments.len() {
            let effect = arguments.get(i).unwrap().get_return(variables).unwrap();
            if let Some((old, other)) = method.arguments.get(i).unwrap()
                .field.field_type.resolve_generic(&effect, syntax,
                placeholder_error("Invalid bounds!".to_string())).await? {
                if let FinalizedTypes::Generic(name, _) = old {
                    manager.mut_generics().insert(name, other);
                } else {
                    panic!("resolve_generic should never return any type other than the generic to replace!");
                }
            }
        }

        // Now all the generic types have been resolved, it's time to replace them with
        // their solidified versions.
        // Degenericed function names have a $ seperating the name and the generics.
        let name = format!("{}${}", method.data.name.split("$").next().unwrap(), display_parenless(
            &manager.generics().values().collect(), "_"));
        // If this function has already been degenericed, use the previous one.
        if syntax.lock().unwrap().functions.types.contains_key(&name) {
            let data = syntax.lock().unwrap().functions.types.get(&name).unwrap().clone();
            return Ok(AsyncDataGetter::new(syntax.clone(), data).await);
        } else {
            // Copy the method and degeneric every type inside of it.
            let mut new_method = CodelessFinalizedFunction::clone(&method);
            // Delete the generics because now they are all solidified.
            new_method.generics.clear();
            let mut method_data = FunctionData::clone(&method.data);
            method_data.name = name.clone();
            new_method.data = Arc::new(method_data);
            // Degeneric the arguments.
            for arguments in &mut new_method.arguments {
                arguments.field.field_type.degeneric(&manager.generics(), syntax,
                                                     placeholder_error("No generic!".to_string()),
                                                     placeholder_error("Invalid bounds!".to_string())).await?;
            }

            // Degeneric the return type if there is one.
            if let Some(returning) = &mut new_method.return_type {
                returning.degeneric(&manager.generics(), syntax,
                                    placeholder_error("No generic!".to_string()),
                                    placeholder_error("Invalid bounds!".to_string())).await?;
            }

            // Add the new degenericed static data to the locked function.
            let original = method;
            let new_method = Arc::new(new_method);
            let mut locked = syntax.lock().unwrap();
            locked.functions.types.insert(name, new_method.data.clone());
            locked.functions.data.insert(new_method.data.clone(), new_method.clone());

            // Spawn a thread to asynchronously degeneric the code inside the function.
            let handle = manager.handle().clone();
            handle.spawn(degeneric_code(syntax.clone(), original, new_method.clone(), manager));
            return Ok(new_method);
        };
    }
}

/// A placeholder error until the actual tokens are passed.
fn placeholder_error(error: String) -> ParsingError {
    return ParsingError::new(String::new(), (0, 0), 0, (0, 0), 0, error);
}

/// Degenerics the code body of the method.
async fn degeneric_code(syntax: Arc<Mutex<Syntax>>, original: Arc<CodelessFinalizedFunction>,
                        degenericed_method: Arc<CodelessFinalizedFunction>, manager: Box<dyn ProcessManager>) {
    // This has to wait until the original is ready to be compiled.
    // Can be improved in the future to use a waiter.
    while !syntax.lock().unwrap().compiling.read().unwrap().contains_key(&original.data.name) {
        thread::yield_now();
    }

    // Gets a clone of the code of the original.
    let code = syntax.lock().unwrap().compiling.read().unwrap().get(&original.data.name).unwrap().code.clone();

    let mut variables = SimpleVariableManager::for_function(degenericed_method.deref());
    // Degenerics the code body.
    let code = match code.degeneric(&manager, &mut variables, &syntax).await {
        Ok(inner) => inner,
        Err(error) => panic!("Error degenericing code: {}", error)
    };

    // Combines the degenericed function with the degenericed code to finalize it.
    let output = CodelessFinalizedFunction::clone(degenericed_method.deref())
        .add_code(code);

    // Sends the finalized function to be compiled.
    syntax.lock().unwrap().compiling.write().unwrap().insert(output.data.name.clone(), Arc::new(output));
}

/// A finalized function, which is ready to be compiled and has been checked of any errors.
#[derive(Clone, Debug)]
pub struct FinalizedFunction {
    pub generics: IndexMap<String, Vec<FinalizedTypes>>,
    pub fields: Vec<FinalizedMemberField>,
    pub code: FinalizedCodeBody,
    pub return_type: Option<FinalizedTypes>,
    pub data: Arc<FunctionData>,
}

impl FinalizedFunction {
    /// Recreates the CodelessFinalizedFunction
    pub fn to_codeless(&self) -> CodelessFinalizedFunction {
        return CodelessFinalizedFunction {
            generics: self.generics.clone(),
            arguments: self.fields.clone(),
            return_type: self.return_type.clone(),
            data: self.data.clone(),
        };
    }
}

/// A body of code, each body must have a label for jump effects to jump to.
/// ! Each nested CodeBody MUST have a jump or return or else the compiler will error !
#[derive(Clone, Default, Debug)]
pub struct CodeBody {
    pub label: String,
    pub expressions: Vec<Expression>,
}

/// A finalized body of code.
#[derive(Clone, Default, Debug)]
pub struct FinalizedCodeBody {
    pub label: String,
    pub expressions: Vec<FinalizedExpression>,
    pub returns: bool,
}

impl CodeBody {
    pub fn new(expressions: Vec<Expression>, label: String) -> Self {
        return Self {
            label,
            expressions,
        };
    }
}

impl FinalizedCodeBody {
    pub fn new(expressions: Vec<FinalizedExpression>, label: String, returns: bool) -> Self {
        return Self {
            label,
            expressions,
            returns,
        };
    }

    /// Degenerics every effect inside the body of code.
    pub async fn degeneric(mut self, process_manager: &Box<dyn ProcessManager>,
                           variables: &mut SimpleVariableManager, syntax: &Arc<Mutex<Syntax>>)
        -> Result<FinalizedCodeBody, ParsingError> {
        for expression in &mut self.expressions {
            expression.effect.degeneric(process_manager, variables, syntax).await?;
        }

        return Ok(self);
    }
}

/// Helper functions to display types.
pub fn display<T>(input: &Vec<T>, deliminator: &str) -> String where T: Display {
    if input.is_empty() {
        return "()".to_string();
    }

    let mut output = String::new();
    for element in input {
        output += &*format!("{}{}", element, deliminator);
    }

    return format!("({})", (&output[..output.len() - deliminator.len()]).to_string());
}

pub fn display_parenless<T>(input: &Vec<T>, deliminator: &str) -> String where T: Display {
    if input.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    for element in input {
        output += &*format!("{}{}", element, deliminator);
    }

    return (&output[..output.len() - deliminator.len()]).to_string();
}

pub fn debug_parenless<T>(input: &Vec<T>, deliminator: &str) -> String where T: Debug {
    if input.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    for element in input {
        output += &*format!("{:?}{}", element, deliminator);
    }

    return (&output[..output.len() - deliminator.len()]).to_string();
}

impl Hash for FunctionData {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl PartialEq for FunctionData {
    fn eq(&self, other: &Self) -> bool {
        return self.name == other.name;
    }
}

impl Eq for FunctionData {}