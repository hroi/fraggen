use apollo_compiler::diagnostics::DiagnosticData;
use apollo_compiler::ApolloCompiler;
use indoc::indoc;
use std::str::from_utf8;

fn assert_valid(output: &str) {
    let mut compiler = ApolloCompiler::new();
    compiler.add_document(output, "generated.graphql");
    for diag in compiler.validate() {
        if let DiagnosticData::UnusedFragment { name: _ } = *diag.data {
            continue;
        }
        eprintln!("{diag}");
        eprintln!("{diag:?}");
        assert!(!diag.data.is_error());
    }
}

#[test]
fn test_single_level() {
    let schema = indoc! {"
        type Foo {
          id: ID
          text: String
          count: Int
          size: Float
          ok: Boolean
        }
    "};
    let expected = indoc! {"
        fragment MyFooFields on Foo {
          __typename
          id
          text
          count
          size
          ok
        }
    "};

    let mut output = Vec::new();
    crate::generate(&schema, &mut output, "My", "Fields", true, true).unwrap();
    let fragments = from_utf8(&output).unwrap();

    assert_valid(fragments);
    assert_eq!(expected, fragments);
}

#[test]
fn test_multilevel() {
    let schema = indoc! {"
        type Foo {
          bar: Bar
        }

        type Bar {
          baz: Int
        }
    "};
    let expected = indoc! {"
        fragment MyFooFields on Foo {
          __typename
          # bar {
          #   ...MyBarFields
          # }
        }

        fragment MyBarFields on Bar {
          __typename
          baz
        }
    "};

    let mut output = Vec::new();
    crate::generate(&schema, &mut output, "My", "Fields", true, true).unwrap();
    let fragments = from_utf8(&output).unwrap();

    assert_valid(fragments);
    assert_eq!(expected, fragments);
}

#[test]
fn test_implements_interface() {
    let schema = indoc! {"
        interface Rect {
          width: Float
          height: Float
        }

        type Box implements Rect {
          id: ID
          width: Float
          height: Float
        }
    "};
    let expected = indoc! {"
        fragment MyBoxFields on Box {
          __typename
          ...MyRectFields
          id
        }

        fragment MyRectFields on Rect {
          width
          height
        }
    "};

    let mut output = Vec::new();
    crate::generate(&schema, &mut output, "My", "Fields", true, true).unwrap();
    let fragments = from_utf8(&output).unwrap();

    assert_valid(fragments);
    assert_eq!(expected, fragments);
}

#[test]
fn test_arguments() {
    let schema = indoc! {"
        type Query {
          searchBeer(name: String!, top: Int): [ID]
        }
    "};
    let expected = indoc! {"
        fragment MyQueryFields on Query {
          __typename
          searchBeer (
            name: $name
            top: $top
          )
        }
    "};

    let mut output = Vec::new();
    crate::generate(&schema, &mut output, "My", "Fields", true, true).unwrap();
    let fragments = from_utf8(&output).unwrap();

    assert_valid(fragments);
    assert_eq!(expected, fragments);
}

#[test]
fn test_input() {
    let schema = indoc! {"
        enum Style {
          PILSNER
          STOUT
        }

        input BeerInput {
          name: String
          style: Style
          abv: Float
        }

        type Mutation {
          postBeer(beer: BeerInput, submitter: ID): ID
        }
    "};
    let expected = indoc! {"
        fragment MyMutationFields on Mutation {
          __typename
          postBeer (
            beer: {
              name: $name
              style: $style
              abv: $abv
            }
            submitter: $submitter
          )
        }
    "};

    let mut output = Vec::new();
    crate::generate(&schema, &mut output, "My", "Fields", true, true).unwrap();
    let fragments = from_utf8(&output).unwrap();

    assert_valid(fragments);
    assert_eq!(expected, fragments);
}
