use apollo_parser::{
    ast::{
        ArgumentsDefinition, Definition, Document, FieldsDefinition, ImplementsInterfaces,
        InputFieldsDefinition, InputObjectTypeDefinition, InterfaceTypeDefinition, NamedType,
        ObjectTypeDefinition, Type, UnionTypeDefinition,
    },
    SyntaxTree,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::prelude::*,
};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FragmentGeneratorError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error at index {0}: \"{1}\", {2}")]
    Parse(usize, String, String),

    #[error("Schema error: {0}")]
    Schema(&'static str),
}

struct FragmentGenerator<W: Write> {
    syntax_tree: Document,
    output: W,

    resolved: HashMap<String, Vec<String>>,
    scalar_type_names: HashSet<String>,

    prefix: String,
    suffix: String,

    add_typename: bool,
}

pub fn generate<W: Write>(
    schema_content: &str,
    output: W,
    prefix: &str,
    suffix: &str,
    add_typename: bool,
) -> Result<(), FragmentGeneratorError> {
    let parser = apollo_parser::Parser::new(schema_content);
    let ast = parser.parse();
    if let Some(err) = ast.errors().next() {
        return Err(FragmentGeneratorError::Parse(
            err.index(),
            err.data().into(),
            err.message().into(),
        ));
    }

    FragmentGenerator::new(ast, output, prefix, suffix, add_typename).execute()
}

impl<W: Write> FragmentGenerator<W> {
    fn new(ast: SyntaxTree, output: W, prefix: &str, suffix: &str, add_typename: bool) -> Self {
        Self {
            syntax_tree: ast.document(),
            output,
            resolved: HashMap::new(),
            scalar_type_names: HashSet::from([
                "Int".into(),
                "Float".into(),
                "String".into(),
                "ID".into(),
                "Boolean".into(),
            ]),
            prefix: prefix.into(),
            suffix: suffix.into(),
            add_typename,
        }
    }

    fn execute(&mut self) -> Result<(), FragmentGeneratorError> {
        let mut enum_typedefs = Vec::new();
        let mut scalar_typedefs = Vec::new();
        let mut iface_typedefs = VecDeque::new();
        let mut union_typedefs = Vec::new();
        let mut obj_typedefs = Vec::new();
        let mut input_obj_typedefs = Vec::new();

        for def in self.syntax_tree.definitions() {
            use Definition::*;
            match def {
                EnumTypeDefinition(typedef) => enum_typedefs.push(typedef),
                ScalarTypeDefinition(typedef) => scalar_typedefs.push(typedef),
                InterfaceTypeDefinition(typedef) => iface_typedefs.push_back(typedef),
                UnionTypeDefinition(typedef) => union_typedefs.push(typedef),
                ObjectTypeDefinition(typedef) => obj_typedefs.push(typedef),
                InputObjectTypeDefinition(typedef) => input_obj_typedefs.push(typedef),
                _ => (),
            }
        }

        // 1. enum types

        for enum_typedef in enum_typedefs {
            let type_name = enum_typedef
                .name()
                .ok_or(FragmentGeneratorError::Schema("scalar has no name"))?
                .text()
                .to_string();
            self.scalar_type_names.insert(type_name);
        }

        // 2. scalar types

        for scalar_typedef in scalar_typedefs {
            let type_name = scalar_typedef
                .name()
                .ok_or(FragmentGeneratorError::Schema("scalar has no name"))?
                .text()
                .to_string();
            self.scalar_type_names.insert(type_name);
        }

        // 3. interface types

        let mut recursions = 0;
        'outer: while let Some(iface_typedef) = iface_typedefs.pop_front() {
            let Some(implements_interfaces) = iface_typedef.implements_interfaces() else {
                self.write_interface_fragment(&iface_typedef)?;
                continue;
            };
            for implements_interface in implements_interfaces.named_types() {
                let type_name = implements_interface
                    .name()
                    .ok_or(FragmentGeneratorError::Schema("named type has no name"))?
                    .text();
                if !self.resolved.contains_key(type_name.as_str()) {
                    recursions += 1;
                    if recursions > iface_typedefs.capacity() {
                        return Err(FragmentGeneratorError::Schema(
                            "recursion limit reached while resolving interface hierarchy",
                        ));
                    }
                    iface_typedefs.push_back(iface_typedef);
                    continue 'outer;
                }
            }
            self.write_interface_fragment(&iface_typedef)?;
        }

        // 4. Union types

        for union_typedef in union_typedefs {
            self.write_union_fragment(&union_typedef)?;
        }

        // 5. object types

        for obj_typedef in obj_typedefs {
            self.write_object_fragment(&obj_typedef)?;
        }

        // 6. Input object types

        for input_obj_typedef in input_obj_typedefs {
            self.write_input_object_fragment(&input_obj_typedef)?;
        }

        Ok(())
    }

    fn inherited_fields(
        &self,
        implements_interfaces: Option<ImplementsInterfaces>,
    ) -> Result<HashSet<String>, FragmentGeneratorError> {
        let mut ret = HashSet::new();

        let Some(implements_interfaces) = implements_interfaces else {
            return Ok(ret);
        };

        for named_type in implements_interfaces.named_types() {
            let type_name = named_type
                .name()
                .ok_or(FragmentGeneratorError::Schema("named type has no name"))?;
            let type_name = type_name.text();
            let fields =
                self.resolved
                    .get(type_name.as_str())
                    .ok_or(FragmentGeneratorError::Schema(
                        "couldn't resolve implemented interface type",
                    ))?;
            for field in fields {
                ret.insert(field.clone());
            }
        }

        Ok(ret)
    }

    fn fragment_name(&self, type_name: &str) -> String {
        format!("{}{}{}", self.prefix, type_name, self.suffix)
    }

    fn write_interface_fragment(
        &mut self,
        typedef: &InterfaceTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        let type_name = &typedef
            .name()
            .ok_or(FragmentGeneratorError::Schema("interface type has no name"))?
            .text()
            .to_string();
        let fragment_name = self.fragment_name(type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;

        if let Some(implements_interfaces) = typedef.implements_interfaces() {
            for named_type in implements_interfaces.named_types() {
                let inherited_type_name = named_type
                    .name()
                    .ok_or(FragmentGeneratorError::Schema("named type has no name"))?
                    .text();
                let fragment_name = self.fragment_name(&inherited_type_name);
                writeln!(self.output, "  ...{fragment_name}")?;
            }
        }

        let own_fields =
            self.write_fields(typedef.fields_definition(), typedef.implements_interfaces())?;
        self.resolved.insert(type_name.clone(), own_fields);

        writeln!(self.output, "}}")?;
        writeln!(self.output)?;
        Ok(())
    }

    fn write_union_fragment(
        &mut self,
        typedef: &UnionTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        let type_name = &typedef
            .name()
            .ok_or(FragmentGeneratorError::Schema("interface type has no name"))?
            .text()
            .to_string();
        let fragment_name = self.fragment_name(type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;

        let union_member_types = typedef
            .union_member_types()
            .ok_or(FragmentGeneratorError::Schema("union has no members"))?;
        for named_type in union_member_types.named_types() {
            let type_name = named_type
                .name()
                .ok_or(FragmentGeneratorError::Schema("named type has no name"))?
                .text();
            let fragment_name = self.fragment_name(type_name.as_str());
            writeln!(self.output, "  ... on {type_name} {{")?;
            writeln!(self.output, "    ...{fragment_name} {{")?;
            writeln!(self.output, "  }}")?;
        }

        writeln!(self.output, "}}")?;
        writeln!(self.output)?;
        Ok(())
    }

    fn write_object_fragment(
        &mut self,
        typedef: &ObjectTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        let type_name = typedef
            .name()
            .ok_or(FragmentGeneratorError::Schema("object type has no name"))?
            .text();
        let fragment_name = self.fragment_name(&type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;
        if self.add_typename {
            writeln!(self.output, "  __typename")?;
        }

        if let Some(implements_interfaces) = typedef.implements_interfaces() {
            for named_type in implements_interfaces.named_types() {
                let inherited_type_name = named_type
                    .name()
                    .ok_or(FragmentGeneratorError::Schema("named type has no name"))?
                    .text();
                let fragment_name = self.fragment_name(&inherited_type_name);
                writeln!(self.output, "  ...{fragment_name}")?;
            }
        }

        let own_fields =
            self.write_fields(typedef.fields_definition(), typedef.implements_interfaces())?;
        self.resolved.insert(type_name.into(), own_fields);

        writeln!(self.output, "}}")?;
        writeln!(self.output)?;
        Ok(())
    }

    fn write_input_object_fragment(
        &mut self,
        typedef: &InputObjectTypeDefinition,
    ) -> Result<(), FragmentGeneratorError> {
        let type_name = typedef
            .name()
            .ok_or(FragmentGeneratorError::Schema("object type has no name"))?
            .text();
        let fragment_name = self.fragment_name(&type_name);
        writeln!(self.output, "fragment {fragment_name} on {type_name} {{")?;
        if self.add_typename {
            writeln!(self.output, "  __typename")?;
        }

        let own_fields = self.write_input_fields(typedef.input_fields_definition())?;
        self.resolved.insert(type_name.into(), own_fields);

        writeln!(self.output, "}}")?;
        writeln!(self.output)?;
        Ok(())
    }

    fn write_input_fields(
        &mut self,
        fields_definition: Option<InputFieldsDefinition>,
    ) -> Result<Vec<String>, FragmentGeneratorError> {
        let mut own_fields = Vec::new();
        let fields_definition = fields_definition
            .ok_or(FragmentGeneratorError::Schema("input object has no fields"))?;
        for field_definition in fields_definition.input_value_definitions() {
            let field_name = field_definition
                .name()
                .ok_or(FragmentGeneratorError::Schema("field has no name"))?
                .text();
            let ty = field_definition
                .ty()
                .ok_or(FragmentGeneratorError::Schema("field has no type"))?;
            self.write_field(&field_name, ty, None)?;
            own_fields.push(field_name.to_string());
        }
        Ok(own_fields)
    }

    fn write_fields(
        &mut self,
        fields_definition: Option<FieldsDefinition>,
        implements_interfaces: Option<ImplementsInterfaces>,
    ) -> Result<Vec<String>, FragmentGeneratorError> {
        let fields_definition =
            fields_definition.ok_or(FragmentGeneratorError::Schema("type has no fields"))?;
        let inherited_fields = self.inherited_fields(implements_interfaces)?;
        let mut own_fields = Vec::new();
        for field_definition in fields_definition.field_definitions() {
            let field_name = field_definition
                .name()
                .ok_or(FragmentGeneratorError::Schema("field has no name"))?
                .text();
            let is_inherited = inherited_fields.contains(field_name.as_str());
            if is_inherited {
                continue;
            }
            let ty = field_definition
                .ty()
                .ok_or(FragmentGeneratorError::Schema("field has no type"))?;
            self.write_field(&field_name, ty, field_definition.arguments_definition())?;
            own_fields.push(field_name.to_string());
            // }
        }
        Ok(own_fields)
    }

    fn write_field(
        &mut self,
        field_name: &str,
        mut ty: Type,
        arguments_definition: Option<ArgumentsDefinition>,
    ) -> Result<(), FragmentGeneratorError> {
        loop {
            match ty {
                Type::NamedType(named_type) => {
                    return self.write_field_with_named_type(
                        field_name,
                        named_type,
                        arguments_definition,
                    );
                }
                Type::ListType(list_type) => {
                    ty = list_type
                        .ty()
                        .ok_or(FragmentGeneratorError::Schema("list has no type"))?;
                }
                Type::NonNullType(non_null_type) => {
                    if let Some(list_type) = non_null_type.list_type() {
                        ty = list_type
                            .ty()
                            .ok_or(FragmentGeneratorError::Schema("list has no type"))?;
                        continue;
                    }
                    let named_type = non_null_type
                        .named_type()
                        .ok_or(FragmentGeneratorError::Schema("non-null has no named type"))?;
                    return self.write_field_with_named_type(
                        field_name,
                        named_type,
                        arguments_definition,
                    );
                }
            }
        }
    }

    fn write_field_with_named_type(
        &mut self,
        field_name: &str,
        named_type: NamedType,
        arguments_definition: Option<ArgumentsDefinition>,
    ) -> Result<(), FragmentGeneratorError> {
        let type_name = named_type
            .name()
            .ok_or(FragmentGeneratorError::Schema("named type has no name"))?
            .text();

        let arglist = if let Some(arguments_definition) = arguments_definition {
            let mut args = Vec::new();
            for input_value_definition in arguments_definition.input_value_definitions() {
                let arg_name = input_value_definition
                    .name()
                    .ok_or(FragmentGeneratorError::Schema("argument has no name"))?
                    .text();
                args.push(format!("{arg_name}: ${arg_name}"));
            }
            format!("({})", args.join(", "))
        } else {
            String::new()
        };

        if self.scalar_type_names.contains(type_name.as_str()) {
            writeln!(self.output, "  {field_name}{arglist}")?;
        } else {
            let fragment_name = self.fragment_name(&type_name);
            writeln!(self.output, "  # {field_name}{arglist} {{")?;
            writeln!(self.output, "  #   ...{fragment_name}")?;
            writeln!(self.output, "  # }}")?;
        }
        Ok(())
    }
}
