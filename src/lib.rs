use apollo_compiler::hir::{InputObjectTypeDefinition, Type, TypeDefinition};
use apollo_compiler::{
    hir::{
        ArgumentsDefinition, FieldDefinition, ImplementsInterface, InputValueDefinition,
        InterfaceTypeDefinition, ObjectTypeDefinition, UnionTypeDefinition,
    },
    ApolloCompiler, HirDatabase,
};
use std::{collections::HashSet, io::prelude::*, result};
use thiserror::Error;

/// # Errors
/// Will return `Err` if there are errors parsing the schema, types can not be resolved
/// or a field type is not supported.
pub fn generate<W: Write>(
    schema_content: &str,
    output: W,
    prefix: &str,
    suffix: &str,
    add_typename: bool,
    quiet: bool,
) -> FraggenResult<()> {
    let mut compiler = apollo_compiler::ApolloCompiler::new();
    compiler.add_type_system(schema_content, "schema.graphql");

    for diagnostic in compiler.validate() {
        if diagnostic.data.is_error() {
            return Err(FragmentGeneratorError::Parse(format!("{diagnostic}")));
        }
        if !quiet && diagnostic.data.is_warning() {
            eprintln!("{diagnostic}");
        }
        if !quiet && diagnostic.data.is_advice() {
            eprintln!("{diagnostic}");
        }
    }

    FragmentGenerator::new(compiler, output, prefix, suffix, add_typename).execute()
}

#[derive(Error, Debug)]
pub enum FragmentGeneratorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Format error: {0}")]
    Fmt(#[from] std::fmt::Error),

    #[error("{0}")]
    Parse(String),

    #[error("Schema error: {0}")]
    Schema(&'static str),
}

type FraggenResult<T> = result::Result<T, FragmentGeneratorError>;

struct FragmentGenerator<W: Write> {
    compiler: ApolloCompiler,
    output: W,
    prefix: String,
    suffix: String,
    add_typename: bool,
    write_newline: bool,
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
            write_newline: false,
        }
    }

    fn execute(&mut self) -> FraggenResult<()> {
        let type_system = self.compiler.db.type_system();

        for (_name, typedef) in type_system.type_definitions_by_name.iter() {
            match typedef {
                TypeDefinition::ObjectTypeDefinition(typedef) if !typedef.is_introspection() => {
                    self.write_object_fragment(typedef)?;
                }
                TypeDefinition::InterfaceTypeDefinition(typedef) => {
                    self.write_interface_fragment(typedef)?;
                }
                TypeDefinition::UnionTypeDefinition(typedef) => {
                    self.write_union_fragment(typedef)?;
                }
                _ => continue,
            }
        }

        Ok(())
    }

    fn write_interface_fragment(&mut self, typedef: &InterfaceTypeDefinition) -> FraggenResult<()> {
        self.write_fragment(
            typedef.name(),
            typedef.implements_interfaces(),
            typedef.fields(),
            false,
        )
    }

    fn write_object_fragment(&mut self, typedef: &ObjectTypeDefinition) -> FraggenResult<()> {
        self.write_fragment(
            typedef.name(),
            typedef.implements_interfaces(),
            typedef.fields(),
            self.add_typename,
        )
    }

    fn write_union_fragment(&mut self, typedef: &UnionTypeDefinition) -> FraggenResult<()> {
        if self.write_newline {
            writeln!(self.output)?;
        } else {
            self.write_newline = true;
        }
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
    ) -> FraggenResult<()> {
        if self.write_newline {
            writeln!(self.output)?;
        } else {
            self.write_newline = true;
        }
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

    fn write_field(&mut self, field: &FieldDefinition) -> FraggenResult<()> {
        let field_name = field.name();
        let mut field_type = field.ty();

        while let Type::NonNull { ty, loc: _ } | Type::List { ty, loc: _ } = field_type {
            field_type = ty;
        }

        let field_type_definition = field_type
            .type_def(&self.compiler.db)
            .ok_or(FragmentGeneratorError::Schema("unresolved field type"))?;

        match field_type_definition {
            TypeDefinition::EnumTypeDefinition(_) | TypeDefinition::ScalarTypeDefinition(_) => {
                self.write_simple_field(field_name, field.arguments())?;
            }
            TypeDefinition::ObjectTypeDefinition(typedef) => {
                self.write_complex_field(field_name, typedef.name(), field.arguments())?;
            }
            TypeDefinition::InterfaceTypeDefinition(typedef) => {
                self.write_complex_field(field_name, typedef.name(), field.arguments())?;
            }
            TypeDefinition::UnionTypeDefinition(typedef) => {
                self.write_complex_field(field_name, typedef.name(), field.arguments())?;
            }
            TypeDefinition::InputObjectTypeDefinition(_) => {
                Err(FragmentGeneratorError::Schema("unsupported field type"))?;
            }
        };
        Ok(())
    }

    fn write_simple_field(
        &mut self,
        field_name: &str,
        arguments: &ArgumentsDefinition,
    ) -> FraggenResult<()> {
        let arglist = self.format_arglist(arguments.input_values(), "  ")?;
        writeln!(self.output, "  {field_name}{arglist}")?;
        Ok(())
    }

    fn write_complex_field(
        &mut self,
        field_name: &str,
        type_name: &str,
        arguments: &ArgumentsDefinition,
    ) -> FraggenResult<()> {
        let fragment_name = self.fragment_name(type_name);
        let arglist = self.format_arglist(arguments.input_values(), "  # ")?;
        writeln!(self.output, "  # {field_name}{arglist} {{")?;
        writeln!(self.output, "  #   ...{fragment_name}")?;
        writeln!(self.output, "  # }}")?;
        Ok(())
    }

    fn fragment_name(&self, type_name: &str) -> String {
        format!("{}{}{}", self.prefix, type_name, self.suffix)
    }

    fn format_arglist(
        &self,
        input_values: &[InputValueDefinition],
        prefix: &str,
    ) -> FraggenResult<String> {
        if input_values.is_empty() {
            Ok(String::new())
        } else {
            let args = input_values
                .iter()
                .map(|arg| self.format_arg(arg, prefix))
                .collect::<FraggenResult<Vec<String>>>()?;
            let join_str = format!("\n{prefix}  ");
            Ok(format!(" (\n{prefix}  {}\n{prefix})", args.join(&join_str)))
        }
    }

    fn format_arg(
        &self,
        input_value: &InputValueDefinition,
        prefix: &str,
    ) -> FraggenResult<String> {
        let mut input_value_type = input_value.ty();
        while let Type::NonNull { ty, loc: _ } | Type::List { ty, loc: _ } = input_value_type {
            input_value_type = ty;
        }

        let typedef = input_value_type
            .type_def(&self.compiler.db)
            .ok_or(FragmentGeneratorError::Schema("unresolved argument type"))?;

        match typedef {
            TypeDefinition::ScalarTypeDefinition(_) | TypeDefinition::EnumTypeDefinition(_) => {
                Ok(format!("{0}: ${}", input_value.name()))
            }
            TypeDefinition::InputObjectTypeDefinition(input_obj_typedef) => Ok(
                Self::format_input_arg(input_value, &input_obj_typedef, prefix),
            ),
            TypeDefinition::ObjectTypeDefinition(_) => todo!(),
            TypeDefinition::InterfaceTypeDefinition(_) => todo!(),
            TypeDefinition::UnionTypeDefinition(_) => todo!(),
        }
    }

    fn format_input_arg(
        input_value_def: &InputValueDefinition,
        typedef: &InputObjectTypeDefinition,
        prefix: &str,
    ) -> String {
        let join_str = format!("\n{prefix}    ");
        let args = typedef
            .fields()
            .map(|field| format!("{0}: ${0}", field.name()))
            .collect::<Vec<String>>()
            .join(&join_str);
        format!(
            "{0}: {{\n{prefix}    {args}\n{prefix}  }}",
            input_value_def.name()
        )
    }
}
