use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use crate::{DisplayIndented, is_modifier, Modifier, to_modifiers};
use crate::code::MemberField;
use crate::function::{display, display_joined, display_parenless};
use crate::type_resolver::FinalizedTypeResolver;
use crate::types::ResolvableTypes;

#[derive(Clone)]
pub struct Struct {
    pub modifiers: u8,
    pub generics: Vec<(String, Vec<ResolvableTypes>)>,
    pub resolved_generics: Vec<(String, ResolvableTypes)>,
    pub fields: Option<Vec<MemberField>>,
    pub functions: Vec<String>,
    pub name: String
}

impl Struct {
    pub fn new(fields: Option<Vec<MemberField>>, generics: Vec<(String, Vec<ResolvableTypes>)>,
               functions: Vec<String>, modifiers: u8, name: String) -> Self {
        return Self {
            modifiers,
            generics,
            resolved_generics: Vec::new(),
            fields,
            functions,
            name
        }
    }

    pub fn finalize(&mut self, type_resolver: &mut dyn FinalizedTypeResolver) {
        if self.fields.is_some() {
            for field in self.fields.as_mut().unwrap() {
                field.field.finalize(type_resolver);
            }
        }
    }

    pub fn resolve_generics(&self, type_resolver: &mut dyn FinalizedTypeResolver, generics: &Vec<ResolvableTypes>) -> Self {
        if generics.len() != self.generics.len() {
            panic!("Missing correct amount of generics for generic function!");
        }
        let mut values = HashMap::new();
        for i in 0..generics.len() {
            let (name, bounds) = self.generics.get(i).unwrap();
            let testing = generics.get(i).unwrap();
            for bound in bounds {
                if !testing.unwrap().is_type(bound.unwrap()) {
                    panic!("Expected {} to be of type {}", testing, bound);
                }
            }
            values.insert(name.clone(), testing.clone());
        }

        let mut returning = self.clone();
        returning.name = self.get_mangled_name(&generics.iter().map(|generic| generic.name().clone()).collect()).clone();
        if let Some(fields) = &returning.fields {
            for field in fields {
                field.field.set_generics(type_resolver, &values);
            }
        }
        for i in 0..returning.generics.len() {
            returning.resolved_generics.push(
                (returning.generics.get(i).unwrap().0.clone(), generics.get(i).unwrap().clone()));
        }
        returning.generics = Vec::new();

        return returning;
    }

    pub fn get_mangled_name(&self, generics: &Vec<String>) -> String {
        return self.name.clone() + "_" + &display_parenless(generics, "_");
    }

    pub fn format(&self, indent: &str, f: &mut Formatter<'_>, type_manager: &dyn FinalizedTypeResolver) -> std::fmt::Result {
        if is_modifier(self.modifiers, Modifier::Trait) {
            write!(f, "{} trait {}", display_joined(&to_modifiers(self.modifiers ^ Modifier::Trait as u8)), self.name)?;
        } else {
            write!(f, "{} struct {}", display_joined(&to_modifiers(self.modifiers)), self.name)?;
        }

        if !self.generics.is_empty() {
            write!(f, "<")?;
            for (name, bounds) in &self.generics {
                write!(f, "{}", name)?;
                if !bounds.is_empty() {
                    write!(f, ": {}", display(bounds, " + "))?;
                }
            }
            write!(f, ">")?;
        }
        write!(f, " {{")?;
        let deeper_indent = "    ".to_string() + indent;
        let deeper_indent = deeper_indent.as_str();

        if self.fields.is_some() {
            for field in self.fields.as_ref().unwrap() {
                write!(f, "\n")?;
                DisplayIndented::format(field, deeper_indent, f)?;
            }
        }

        write!(f, "\n")?;
        for member in &self.functions {
            write!(f, "\n")?;
            DisplayIndented::format(type_manager.get_function(member).unwrap(), deeper_indent, f)?;
            write!(f, "\n")?;
        }
        write!(f, "{}}}", indent)?;
        return Ok(());
    }
}