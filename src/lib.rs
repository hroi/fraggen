use apollo_compiler::{
    hir::{
        ArgumentsDefinition, FieldDefinition, ImplementsInterface, InputValueDefinition,
        InterfaceTypeDefinition, ObjectTypeDefinition, UnionTypeDefinition,
    },
    ApolloCompiler, HirDatabase,
};
use std::{collections::HashSet, io::prelude::*};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FragmentGeneratorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Schema error: {0}")]
    Schema(&'static str),
}

pub fn generate<W: Write>(
    schema_content: &str,
    output: W,
    prefix: &str,
    suffix: &str,
    add_typename: bool,
) -> Result<(), FragmentGeneratorError> {
    let mut compiler = apollo_compiler::ApolloCompiler::new();
    compiler.add_document(schema_content, "schema.graphql");
    let diagnostics = compiler.validate();
    for diagnostic in diagnostics {
        if diagnostic.data.is_error() {
            return Err(FragmentGeneratorError::Parse(format!("{}", diagnostic)));
        }
    }

    FragmentGenerator::new(compiler, output, prefix, suffix, add_typename).execute()
}

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

    fn execute(&mut self) -> Result<(), FragmentGeneratorError> {
        let type_system = self.compiler.db.type_system();

        for (_name, typedef) in type_system.definitions.interfaces.iter() {
            self.write_interface_fragment(typedef)?;
            writeln!(self.output)?;
        }

        for (_name, typedef) in type_system
            .definitions
            .objects
            .iter()
            .filter(|(_, typedef)| !typedef.is_introspection())
        {
            self.write_object_fragment(typedef)?;
            writeln!(self.output)?;
        }

        for (_name, typedef) in type_system.definitions.unions.iter() {
            self.write_union_fragment(typedef)?;
            writeln!(self.output)?;
        }

        Ok(())
    }

    fn write_interface_fragment(
        &mut self,
        typedef: &InterfaceTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        self.write_fragment(
            typedef.name(),
            typedef.implements_interfaces(),
            typedef.fields(),
        )
    }

    fn write_object_fragment(
        &mut self,
        typedef: &ObjectTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        self.write_fragment(
            typedef.name(),
            typedef.implements_interfaces(),
            typedef.fields(),
        )
    }

    fn write_union_fragment(
        &mut self,
        typedef: &UnionTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
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
    ) -> Result<(), FragmentGeneratorError> {
        let fragment_name = self.fragment_name(type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;
        if self.add_typename {
            writeln!(self.output, "  __typename")?;
        }

        let mut inherited_fields = HashSet::new();
        for implements_interface in implements_interfaces {
            let interface_typedef = implements_interface
                .interface_definition(&self.compiler.db)
                .ok_or(FragmentGeneratorError::Schema(
                    "implemented interface could not be resolved",
                ))?;
            inherited_fields.extend(interface_typedef.fields().map(|f| f.name().to_string()));
            let interface_name = implements_interface.interface();
            let fragment_name = self.fragment_name(interface_name);
            writeln!(self.output, "  ...{fragment_name}")?;
        }

        for field in fields {
            use apollo_compiler::hir::Type::*;
            let field_name = field.name();
            if inherited_fields.contains(field_name) {
                continue;
            };
            let mut field_type = field.ty();
            loop {
                match field_type {
                    NonNull { ty, loc: _ } => field_type = ty,
                    List { ty, loc: _ } => field_type = ty,
                    _ => break,
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
                ObjectTypeDefinition(ref typedef) => {
                    self.write_complex_field(field_name, typedef, field.arguments())?
                }
                InterfaceTypeDefinition(_) | UnionTypeDefinition(_) => {
                    let fragment_name = self.fragment_name(&field_type.name());
                    writeln!(self.output, "  # {field_name} {{")?;
                    writeln!(self.output, "  # ...{fragment_name}")?;
                    writeln!(self.output, "  # }}")?;
                }
                other => {
                    dbg!(other);
                    Err(FragmentGeneratorError::Schema("unsupported field type"))?
                }
            };
        }
        writeln!(self.output, "}}")?;
        Ok(())
    }

    fn write_simple_field(
        &mut self,
        field_name: &str,
        arguments: &ArgumentsDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        let arglist = Self::format_arglist(arguments.input_values());
        writeln!(self.output, "  {field_name}{arglist}")?;
        Ok(())
    }

    fn write_complex_field(
        &mut self,
        field_name: &str,
        field_typedef: &ObjectTypeDefinition,
        arguments: &ArgumentsDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        let fragment_name = self.fragment_name(field_typedef.name());
        let arglist = Self::format_arglist(arguments.input_values());
        writeln!(self.output, "  # {field_name}{arglist} {{")?;
        writeln!(self.output, "  # ...{fragment_name}")?;
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
