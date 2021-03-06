use std::f32::EPSILON;
use std::collections::LinkedList;

use serde_json::value::{Value as Json, from_value, to_value};

use context::{Context, JsonRender, JsonNumber, JsonTruthy};
use template::Template;
use errors::TeraResult;
use errors::TeraError::*;
use parser::Node;
use parser::Node::*;
use tera::Tera;


// we need to have some data in the renderer for when we are in a ForLoop
// For example, accessing the local variable would fail when
// looking it up in the context
#[derive(Debug)]
struct ForLoop {
    variable_name: String,
    current: usize,
    values: Vec<Json>
}

impl ForLoop {
    pub fn new(local: String, values: Vec<Json>) -> ForLoop {
        ForLoop {
            variable_name: local,
            current: 0,
            values: values
        }
    }

    pub fn increment(&mut self) {
        self.current += 1;
    }

    pub fn get(&self) -> Option<&Json> {
        self.values.get(self.current)
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
pub struct Renderer<'a> {
    template: &'a Template,
    context: Json,
    tera: &'a Tera,
    for_loops: Vec<ForLoop>,
}

impl<'a> Renderer<'a> {
    pub fn new(tpl: &'a Template, tera: &'a Tera, context: Context) -> Renderer<'a> {
        Renderer {
            template: tpl,
            tera: tera,
            context: context.as_json(),
            for_loops: vec![],
        }
    }

    // Lookup a variable name from the context and takes into
    // account for loops variables
    fn lookup_variable(&self, key: &str) -> TeraResult<Json> {
        // Look in the plain context if we aren't in a for loop
        if self.for_loops.is_empty() {
            return self.context.lookup(key).cloned()
                .ok_or_else(|| FieldNotFound(key.to_string()));
        }

        for for_loop in self.for_loops.iter().rev() {
            if key.starts_with(&for_loop.variable_name) {
                let value = match for_loop.get() {
                    Some(f) => f,
                    None => { return Ok(to_value(&"")); }
                };

                // might be a struct or some nested structure
                if key.contains('.') {
                    let new_key = key.split_terminator('.').skip(1).collect::<Vec<&str>>().join(".");
                    return value.lookup(&new_key).cloned()
                        .ok_or_else(|| FieldNotFound(key.to_string()));
                } else {
                    return Ok(value.clone());
                }
            } else {
                match key {
                    "loop.index" => { return Ok(to_value(&(for_loop.current + 1))); },
                    "loop.index0" => { return Ok(to_value(&for_loop.current)); },
                    "loop.first" => { return Ok(to_value(&(for_loop.current == 0))); },
                    "loop.last" => { return Ok(to_value(&(for_loop.current == for_loop.len() - 1))); },
                    _ => ()
                };
            }
        }

        // dummy statement to satisfy the compiler
        // TODO: make it so that's not needed
        self.context.lookup(key).cloned().ok_or_else(|| FieldNotFound(key.to_string()))
    }

    // Gets an identifier and return its json value
    // If there is no filter, it's itself, otherwise call the filters in order
    // an return their result
    fn eval_ident(&self, node: &Node) -> TeraResult<Json> {
        match *node {
            Identifier { ref name, ref filters } => {
                let mut value = try!(self.lookup_variable(name));
                if let Some(ref _filters) = *filters {
                    for filter in _filters {
                        match *filter {
                            Filter { ref name, ref args, ref args_ident } => {
                                let filter_fn = try!(self.tera.get_filter(name));
                                let mut all_args = args.clone();
                                for (arg_name, ident_name) in args_ident {
                                    all_args.insert(arg_name.to_string(), try!(self.lookup_variable(ident_name)));
                                }
                                value = try!(filter_fn(value, all_args));
                            },
                            _ => unreachable!(),
                        };
                    }
                    return Ok(value);
                }
                Ok(value)
            },
            _ => unreachable!()
        }
    }

    fn eval_math(&self, node: &Node) -> TeraResult<f32> {
        match *node {
            Identifier { ref name, .. } => {
                let value = try!(self.eval_ident(node));
                match value.to_number() {
                    Ok(v) =>  Ok(v),
                    Err(_) => Err(NotANumber(name.to_string()))
                }
            },
            Int(s) => Ok(s as f32),
            Float(s) => Ok(s),
            Math { ref lhs, ref rhs, ref operator } => {
                let l = try!(self.eval_math(lhs));
                let r = try!(self.eval_math(rhs));
                let mut result = match operator.as_str() {
                    "*" => l * r,
                    "/" => l / r,
                    "+" => l + r,
                    "-" => l - r,
                    _ => unreachable!()
                };
                // TODO: fix properly
                // TODO: add tests for float maths arithmetics
                if result.fract() < 0.01 {
                    result = result.round();
                }
                Ok(result)
            }
            _ => unreachable!()
        }
    }

    // TODO: clean up this, too ugly right now for the == and != nodes
    fn eval_condition(&self, node: Node) -> TeraResult<bool> {
        match node {
            // Simple truthiness check
            Identifier { .. } => {
                let value = try!(self.eval_ident(&node));
                Ok(value.is_truthy())
            },
            Logic { lhs, rhs, operator } => {
                match operator.as_str() {
                    "or" => {
                        let result = try!(self.eval_condition(*lhs)) || try!(self.eval_condition(*rhs));
                        return Ok(result);
                    },
                    "and" => {
                        let result = try!(self.eval_condition(*lhs)) && try!(self.eval_condition(*rhs));
                        return Ok(result);
                    },
                    ">=" | ">" | "<=" | "<" => {
                        let l = try!(self.eval_math(&lhs));
                        let r = try!(self.eval_math(&rhs));
                        let result = match operator.as_str() {
                            ">=" => l >= r,
                            ">" => l > r,
                            "<=" => l <= r,
                            "<" => l < r,
                            _ => unreachable!()
                        };
                        return Ok(result);
                    },
                    // This is quite different from the other operators
                    // TODO: clean this up, this is ugly
                    "==" | "!=" => {
                        match *lhs {
                            Logic { .. } => {
                                // let l = self.eval_condition(lhs);
                                // TODO: rhs MUST be bool like
                                panic!("Unimplemented");
                            },
                            Identifier { .. } => {
                                let l = try!(self.eval_ident(&lhs));
                                // who knows what rhs is
                                // Here goes a whole new level of ugliness
                                match *rhs {
                                    Identifier { .. } => {
                                        let r = try!(self.eval_ident(&rhs));
                                        let result = match operator.as_str() {
                                            "==" => l == r,
                                            "!=" => l != r,
                                            _ => unreachable!()
                                        };
                                        return Ok(result);
                                    },
                                    Int(r) => {
                                        let l2: i32 = match from_value(l.clone()) {
                                            Ok(k) => k,
                                            Err(_) => { return Err(NotANumber(l.to_string())); }
                                        };
                                        let result = match operator.as_str() {
                                            "==" => l2 == r,
                                            "!=" => l2 != r,
                                            _ => unreachable!()
                                        };
                                        return Ok(result);
                                    },
                                    Float(r) => {
                                        let l2: f32 = match from_value(l.clone()) {
                                            Ok(k) => k,
                                            Err(_) => { return Err(NotANumber(l.to_string())); }
                                        };
                                        let result = match operator.as_str() {
                                            "==" => (l2 - r).abs() < EPSILON,
                                            "!=" => (l2 - r).abs() > EPSILON,
                                            _ => unreachable!()
                                        };
                                        return Ok(result);
                                    },
                                    _ => unreachable!()
                                }
                            },
                            Int(n) => {
                                // rhs MUST be a number
                                let l = n as f32; // TODO: that's going to cause issues
                                let r = try!(self.eval_math(&rhs));
                                let result = match operator.as_str() {
                                    "==" => (l - r).abs() < EPSILON,
                                    "!=" => (l - r).abs() > EPSILON,
                                    _ => unreachable!()
                                };
                                return Ok(result);
                            },
                            Float(l) => {
                                // rhs MUST be a number
                                let r = try!(self.eval_math(&rhs));
                                let result = match operator.as_str() {
                                    "==" => (l - r).abs() < EPSILON,
                                    "!=" => (l - r).abs() > EPSILON,
                                    _ => unreachable!()
                                };
                                return Ok(result);
                            },
                            Math { .. } => {
                                // rhs MUST be a number
                                let l = try!(self.eval_math(&lhs));
                                let r = try!(self.eval_math(&rhs));
                                let result = match operator.as_str() {
                                    "==" => (l - r).abs() < EPSILON,
                                    "!=" => (l - r).abs() > EPSILON,
                                    _ => unreachable!()
                                };
                                return Ok(result);
                            },
                            _ => unreachable!()
                        }
                    },
                    _ => unreachable!()
                }
                Ok(false)
            },
            _ => unreachable!()
        }
    }

    // eval all the values in a {{ }} block
    fn render_variable_block(&mut self, node: Node) -> TeraResult<String>  {
        match node {
            Identifier { .. } => {
                let value = try!(self.eval_ident(&node));
                Ok(value.render())
            },
            Math { .. } => {
                let result = try!(self.eval_math(&node));
                Ok(result.to_string())
            }
            _ => unreachable!()
        }
    }

    // evaluates conditions and render bodies accordingly
    fn render_if(&mut self, condition_nodes: LinkedList<Node>, else_node: Option<Box<Node>>) -> TeraResult<String> {
        let mut skip_else = false;
        let mut output = String::new();
        for node in condition_nodes {
            match node {
                Conditional {condition, body } => {
                    if try!(self.eval_condition(*condition)) {
                        skip_else = true;
                        // Remove if/elif whitespace
                        output.push_str(try!(self.render_node(*body.clone())).trim_left());
                    }
                },
                _ => unreachable!()
            }
        }

        if skip_else {
            // Remove endif whitespace
            return Ok(output.trim_right().to_string());
        }

        if let Some(e) = else_node {
            // Remove else whitespace
            output.push_str(try!(self.render_node(*e)).trim_left());
        };

        // Remove endif whitespace
        Ok(output.trim_right().to_string())
    }

    fn render_for(&mut self, variable_name: String, array_name: String, body: Box<Node>) -> TeraResult<String> {
        let list = try!(self.lookup_variable(&array_name));

        if !list.is_array() {
            return Err(NotAnArray(array_name.to_string()));
        }

        // Safe unwrap
        let deserialized = list.as_array().unwrap();
        let length = deserialized.len();
        self.for_loops.push(ForLoop::new(variable_name, deserialized.clone()));
        let mut i = 0;
        let mut output = String::new();
        if length > 0 {
            loop {
                output.push_str(try!(self.render_node(*body.clone())).trim_left());
                // Safe unwrap
                self.for_loops.last_mut().unwrap().increment();
                if i == length - 1 {
                    break;
                }
                i += 1;
            }
            output = output.trim_right().to_string();
        }

        Ok(output.trim_right().to_string())
    }

    pub fn render_node(&mut self, node: Node) -> TeraResult<String> {
        match node {
            Text(s) => Ok(s),
            Raw(s) => Ok(s.trim().to_string()),
            VariableBlock(exp) => self.render_variable_block(*exp),
            If {condition_nodes, else_node} => {
                self.render_if(condition_nodes, else_node)
            },
            List(body) => {
                let mut output = String::new();
                for n in body {
                    output.push_str(&try!(self.render_node(n)));
                }
                Ok(output)
            },
            For {variable, array, body} => {
                self.render_for(variable, array, body)
            },
            Block {name, body} => {
                match self.template.blocks.get(&name) {
                    Some(b) => {
                        match b.clone() {
                            Block {body, ..} => {
                                self.render_node(*body.clone())
                            },
                            _ => unreachable!()
                        }
                    },
                    None => {
                        self.render_node(*body)
                    }
                }
            },
            _ => unreachable!()
        }
    }

    pub fn render(&mut self) -> TeraResult<String> {
        let ast = match self.template.parent {
            Some(ref p) => {
                let parent = try!(self.tera.get_template(p));
                parent.ast.get_children()
            },
            None => self.template.ast.get_children()
        };

        let mut output = String::new();
        for node in ast {
            output.push_str(&try!(self.render_node(node)));
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use context::Context;
    use errors::TeraResult;
    use tera::Tera;

    fn render_template(content: &str, context: Context) -> TeraResult<String> {
        let mut tera = Tera::default();
        tera.add_template("hello", content);

        tera.render("hello", context)
    }

    #[test]
    fn test_render_simple_string() {
        let result = render_template("<h1>Hello world</h1>", Context::new());
        assert_eq!(result.unwrap(), "<h1>Hello world</h1>".to_owned());
    }

    #[test]
    fn test_render_math() {
        let result = render_template("This is {{ 2000 + 16 }}.", Context::new());
        assert_eq!(result.unwrap(), "This is 2016.".to_owned());
    }

    #[test]
    fn test_render_basic_variable() {
        let mut context = Context::new();
        context.add("name", &"Vincent");
        let result = render_template("My name is {{ name }}.", context);
        assert_eq!(result.unwrap(), "My name is Vincent.".to_owned());
    }

    #[test]
    fn test_render_math_with_variable() {
        let mut context = Context::new();
        context.add("vat_rate", &0.20);
        let result = render_template("Vat: £{{ 100 * vat_rate }}.", context);

        assert_eq!(result.unwrap(), "Vat: £20.".to_owned());
    }

    #[test]
    fn test_render_comment() {
        let result = render_template("<h1>Hello {# comment #} world</h1>", Context::new());
        assert_eq!(result.unwrap(), "<h1>Hello  world</h1>".to_owned());
    }

    #[test]
    fn test_render_nested_comment() {
        let result = render_template("<h1>Hello {# comment {# nested #} world</h1>", Context::new());
        assert_eq!(result.unwrap(), "<h1>Hello  world</h1>".to_owned());
    }

    #[test]
    fn test_ignore_variable_in_comment() {
        let mut context = Context::new();
        context.add("name", &"Vincent");
        let result = render_template("My name {# was {{ name }} #} is No One.", context);

        assert_eq!(result.unwrap(), "My name  is No One.".to_owned());
    }

    #[test]
    fn test_render_if_simple() {
        let mut context = Context::new();
        context.add("is_admin", &true);
        let result = render_template("{% if is_admin %}Admin{% endif %}", context);

        assert_eq!(result.unwrap(), "Admin".to_owned());
    }

    #[test]
    fn test_render_if_or_conditions() {
        let mut context = Context::new();
        context.add("is_adult", &false);
        context.add("age", &18);
        let result = render_template("{% if is_adult or age + 1 > 18 %}Adult{% endif %}", context);

        assert_eq!(result.unwrap(), "Adult".to_owned());
    }

    #[test]
    fn test_render_if_and_conditions_with_equality() {
        let mut context = Context::new();
        context.add("is_adult", &true);
        context.add("age", &18);
        let result = render_template("{% if is_adult and age == 18 %}Adult{% endif %}", context);

        assert_eq!(result.unwrap(), "Adult".to_owned());
    }

    #[test]
    fn test_render_basic_for() {
        let mut context = Context::new();
        context.add("data", &vec![1,2,3]);
        let result = render_template("{% for i in data %}{{i}}{% endfor %}", context);

        assert_eq!(result.unwrap(), "123".to_owned());
    }

    #[test]
    fn test_render_loop_variables() {
        let mut context = Context::new();
        context.add("data", &vec![1,2,3]);
        let result = render_template(
            "{% for i in data %}{{loop.index}}{{loop.index0}}{{loop.first}}{{loop.last}}{% endfor %}",
            context
        );

        assert_eq!(result.unwrap(), "10truefalse21falsefalse32falsetrue".to_owned());
    }

    #[test]
    fn test_render_filter() {
        let mut context = Context::new();
        context.add("greeting", &"hello");
        let result = render_template(
            "{{ greeting | upper }}",
            context
        );

        assert_eq!(result.unwrap(), "HELLO".to_owned());
    }
}
