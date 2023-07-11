use apollo_compiler::{
    hir::{
        ArgumentsDefinition, FieldDefinition, ImplementsInterface, InputValueDefinition,
        InterfaceTypeDefinition, ObjectTypeDefinition, UnionTypeDefinition,
    },
    ApolloCompiler, HirDatabase,
};
use std::{collections::HashSet, io::prelude::*, result};
use thiserror::Error;

pub fn generate<W: Write>(
    schema_content: &str,
    output: W,
    prefix: &str,
    suffix: &str,
    add_typename: bool,
    quiet: bool,
) -> FraggenResult {
    let mut compiler = apollo_compiler::ApolloCompiler::new();
    compiler.add_type_system(schema_content, "schema.graphql");
    let diagnostics = compiler.validate();
    for diagnostic in diagnostics {
        if diagnostic.data.is_error() {
            return Err(FragmentGeneratorError::Parse(format!("{}", diagnostic)));
        }
        if !quiet && diagnostic.data.is_warning() {
            eprintln!("{}", diagnostic);
        }
        if !quiet && diagnostic.data.is_advice() {
            eprintln!("{}", diagnostic);
        }
    }

    FragmentGenerator::new(compiler, output, prefix, suffix, add_typename).execute()
}

#[derive(Error, Debug)]
pub enum FragmentGeneratorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Parse(String),

    #[error("Schema error: {0}")]
    Schema(&'static str),
}

type FraggenResult = result::Result<(), FragmentGeneratorError>;

struct FragmentGenerator<W: Write> {
    compiler: ApolloCompiler,
    output: W,
    prefix: String,
    suffix: String,
    add_typename: bool,
}

impl<W: Write> FragmentGenerator<W> {
    fn new(
        compiler: ApolloCompiler,
        output: W,
        prefix: &str,
        suffix: &str,
        add_typename: bool,
    ) -> Self {
        Self {
            compiler,
            output,
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
            add_typename,
        }
    }

    fn execute(&mut self) -> FraggenResult {
        let type_system = self.compiler.db.type_system();

        for (_name, typedef) in type_system.type_definitions_by_name.iter() {
            use apollo_compiler::hir::TypeDefinition::*;
            match typedef {
                ObjectTypeDefinition(typedef) if !typedef.is_introspection() => {
                    self.write_object_fragment(typedef)?
                }
                InterfaceTypeDefinition(typedef) => self.write_interface_fragment(typedef)?,
                UnionTypeDefinition(typedef) => self.write_union_fragment(typedef)?,
                _ => continue,
            }
            writeln!(self.output)?;
        }

        Ok(())
    }

    fn write_interface_fragment(&mut self, typedef: &InterfaceTypeDefinition) -> FraggenResult {
        self.write_fragment(
            typedef.name(),
            typedef.implements_interfaces(),
            typedef.fields(),
            false,
        )
    }

    fn write_object_fragment(&mut self, typedef: &ObjectTypeDefinition) -> FraggenResult {
        self.write_fragment(
            typedef.name(),
            typedef.implements_interfaces(),
            typedef.fields(),
            self.add_typename,
        )
    }

    fn write_union_fragment(&mut self, typedef: &UnionTypeDefinition) -> FraggenResult {
        let type_name = typedef.name();
        let fragment_name = self.fragment_name(type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;

        for member in typedef.members() {
            let member_name = member.name();
            let fragment_name = self.fragment_name(member_name);
            writeln!(self.output, "  ... on {member_name} {{")?;
            writeln!(self.output, "    ...{fragment_name}")?;
            writeln!(self.output, "  }}")?;
        }

        writeln!(self.output, "}}")?;
        Ok(())
    }

    fn write_fragment<'a>(
        &mut self,
        type_name: &'a str,
        implements_interfaces: impl Iterator<Item = &'a ImplementsInterface>,
        fields: impl Iterator<Item = &'a FieldDefinition>,
        add_typename: bool,
    ) -> FraggenResult {
        let fragment_name = self.fragment_name(type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;
        if add_typename {
            writeln!(self.output, "  __typename")?;
        }

        let mut inherited_fields = HashSet::new();

        for implements_interface in implements_interfaces {
            let interface_typedef = implements_interface
                .interface_definition(&self.compiler.db)
                .ok_or(FragmentGeneratorError::Schema("unresolved interface"))?;
            inherited_fields.extend(interface_typedef.fields().map(|f| f.name().to_string()));
            let interface_name = implements_interface.interface();
            let fragment_name = self.fragment_name(interface_name);
            writeln!(self.output, "  ...{fragment_name}")?;
        }

        for field in fields.filter(|fld| !inherited_fields.contains(fld.name())) {
            self.write_field(field)?;
        }

        writeln!(self.output, "}}")?;
        Ok(())
    }

    fn write_field(&mut self, field: &FieldDefinition) -> FraggenResult {
        use apollo_compiler::hir::Type::*;
        let field_name = field.name();
        let mut field_type = field.ty();
        loop {
            match field_type {
                NonNull { ty, loc: _ } => field_type = ty,
                List { ty, loc: _ } => field_type = ty,
                Named { name: _, loc: _ } => break,
            }
        }
        let field_type_definition = field_type
            .type_def(&self.compiler.db)
            .ok_or(FragmentGeneratorError::Schema("unresolved field type"))?;

        use apollo_compiler::hir::TypeDefinition::*;
        match field_type_definition {
            EnumTypeDefinition(_) | ScalarTypeDefinition(_) => {
                self.write_simple_field(field_name, field.arguments())?
            }
            ObjectTypeDefinition(typedef) => {
                self.write_complex_field(field_name, typedef.name(), field.arguments())?
            }
            InterfaceTypeDefinition(typedef) => {
                self.write_complex_field(field_name, typedef.name(), field.arguments())?
            }
            UnionTypeDefinition(typedef) => {
                self.write_complex_field(field_name, typedef.name(), field.arguments())?
            }
            _ => Err(FragmentGeneratorError::Schema("unsupported field type"))?,
        };
        Ok(())
    }

    fn write_simple_field(
        &mut self,
        field_name: &str,
        arguments: &ArgumentsDefinition,
    ) -> FraggenResult {
        let arglist = Self::format_arglist(arguments.input_values());
        writeln!(self.output, "  {field_name}{arglist}")?;
        Ok(())
    }

    fn write_complex_field(
        &mut self,
        field_name: &str,
        type_name: &str,
        arguments: &ArgumentsDefinition,
    ) -> FraggenResult {
        let fragment_name = self.fragment_name(type_name);
        let arglist = Self::format_arglist(arguments.input_values());
        writeln!(self.output, "  # {field_name}{arglist} {{")?;
        writeln!(self.output, "  #   ...{fragment_name}")?;
        writeln!(self.output, "  # }}")?;
        Ok(())
    }

    fn fragment_name(&self, type_name: &str) -> String {
        format!("{}{}{}", self.prefix, type_name, self.suffix)
    }

    fn format_arglist(input_values: &[InputValueDefinition]) -> String {
        if input_values.is_empty() {
            String::new()
        } else {
            let args: Vec<String> = input_values
                .iter()
                .map(|arg| format!("{0}: ${0}", arg.name()))
                .collect();
            format!("({})", args.join(", "))
        }
    }
}
