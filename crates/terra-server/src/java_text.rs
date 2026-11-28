//! The pieces of `java.lang` and `java.util.regex` behaviour the ported command parsers rely on.

pub use terra_codec::{java_double_compare, parse_java_double};

/// `Math.round(double)`: nearest `long`, ties toward positive infinity, NaN to 0, saturating.
pub fn java_round(value: f64) -> i64 {
    if value.is_nan() {
        return 0;
    }
    let floor = value.floor();
    let rounded = if value - floor >= 0.5 { floor + 1.0 } else { floor };
    if rounded >= 9.223_372_036_854_776e18 {
        i64::MAX
    } else if rounded <= -9.223_372_036_854_776e18 {
        i64::MIN
    } else {
        rounded as i64
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Literal(char),
    Class(Vec<(char, char)>),
    Digit,
    Space,
    Any(Vec<Node>),
    Sequence(Vec<Node>),
    Group(Box<Node>, Option<usize>),
    Repeat { node: Box<Node>, min: usize, max: usize },
    Start,
    End,
}

/// A backtracking matcher for the regular-expression subset the TerraMinusMinus coordinate
/// patterns use, with `java.util.regex` semantics under `CASE_INSENSITIVE` (ASCII folding):
/// greedy quantifiers, ordered alternation, capture of the last iteration.
#[derive(Debug, Clone)]
pub struct JavaPattern {
    root: Node,
    groups: usize,
}

pub type Captures = Vec<Option<(usize, usize)>>;

impl JavaPattern {
    pub fn compile(pattern: &str) -> Result<Self, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut parser = PatternParser { chars: &chars, position: 0, groups: 0 };
        let root = parser.alternation()?;
        if parser.position != chars.len() {
            return Err(format!("unexpected `{}` at {}", chars[parser.position], parser.position));
        }
        Ok(Self { root, groups: parser.groups })
    }

    /// `Matcher.find()` from the start: the first position where the pattern matches, and the
    /// groups as char ranges of `input`.
    pub fn find(&self, input: &[char]) -> Option<Captures> {
        for start in 0..=input.len() {
            let mut captures = vec![None; self.groups + 1];
            let mut end = None;
            if match_node(&self.root, input, start, &mut captures, &mut |position, _| {
                end = Some(position);
                true
            }) {
                captures[0] = end.map(|end| (start, end));
                return Some(captures);
            }
        }
        None
    }
}

fn fold(c: char) -> char {
    c.to_ascii_lowercase()
}

fn match_sequence(nodes: &[Node], input: &[char], position: usize, captures: &mut Captures, next: &mut dyn FnMut(usize, &mut Captures) -> bool) -> bool {
    match nodes.split_first() {
        None => next(position, captures),
        Some((first, rest)) => match_node(first, input, position, captures, &mut |after, captures| match_sequence(rest, input, after, captures, next)),
    }
}

#[derive(Clone, Copy)]
struct Repetition<'a> {
    node: &'a Node,
    min: usize,
    max: usize,
}

fn match_repeat(repetition: Repetition<'_>, count: usize, input: &[char], position: usize, captures: &mut Captures, next: &mut dyn FnMut(usize, &mut Captures) -> bool) -> bool {
    let Repetition { node, min, max } = repetition;
    if count < max {
        let saved = captures.clone();
        let advanced = match_node(node, input, position, captures, &mut |after, captures| {
            (after != position || count < min) && match_repeat(repetition, count + 1, input, after, captures, next)
        });
        if advanced {
            return true;
        }
        *captures = saved;
    }
    count >= min && next(position, captures)
}

fn match_node(node: &Node, input: &[char], position: usize, captures: &mut Captures, next: &mut dyn FnMut(usize, &mut Captures) -> bool) -> bool {
    let single = |predicate: &dyn Fn(char) -> bool, captures: &mut Captures, next: &mut dyn FnMut(usize, &mut Captures) -> bool| {
        input.get(position).is_some_and(|&c| predicate(c)) && next(position + 1, captures)
    };
    match node {
        Node::Literal(expected) => single(&|c| fold(c) == fold(*expected), captures, next),
        Node::Class(ranges) => single(&|c| ranges.iter().any(|&(low, high)| (low..=high).contains(&c) || (low..=high).contains(&fold(c)) || (low..=high).contains(&c.to_ascii_uppercase())), captures, next),
        Node::Digit => single(&|c| c.is_ascii_digit(), captures, next),
        Node::Space => single(&|c| matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r'), captures, next),
        Node::Start => position == 0 && next(position, captures),
        Node::End => position == input.len() && next(position, captures),
        Node::Sequence(nodes) => match_sequence(nodes, input, position, captures, next),
        Node::Any(options) => options.iter().any(|option| {
            let saved = captures.clone();
            let matched = match_node(option, input, position, captures, next);
            if !matched {
                *captures = saved;
            }
            matched
        }),
        Node::Group(inner, index) => match_node(inner, input, position, captures, &mut |after, captures| {
            let Some(index) = *index else { return next(after, captures) };
            let previous = captures[index];
            captures[index] = Some((position, after));
            next(after, captures) || {
                captures[index] = previous;
                false
            }
        }),
        Node::Repeat { node, min, max } => match_repeat(Repetition { node, min: *min, max: *max }, 0, input, position, captures, next),
    }
}

struct PatternParser<'a> {
    chars: &'a [char],
    position: usize,
    groups: usize,
}

impl PatternParser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn alternation(&mut self) -> Result<Node, String> {
        let mut options = vec![self.sequence()?];
        while self.peek() == Some('|') {
            self.position += 1;
            options.push(self.sequence()?);
        }
        Ok(if options.len() == 1 { options.remove(0) } else { Node::Any(options) })
    }

    fn sequence(&mut self) -> Result<Node, String> {
        let mut nodes = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            nodes.push(self.quantified(atom)?);
        }
        Ok(Node::Sequence(nodes))
    }

    fn number(&mut self) -> Result<usize, String> {
        let start = self.position;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.position += 1;
        }
        self.chars[start..self.position].iter().collect::<String>().parse().map_err(|_| "bad repetition count".to_owned())
    }

    fn quantified(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('?') => (0, 1),
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('{') => {
                self.position += 1;
                let min = self.number()?;
                let max = if self.peek() == Some(',') {
                    self.position += 1;
                    self.number()?
                } else {
                    min
                };
                if self.peek() != Some('}') {
                    return Err("unterminated repetition".into());
                }
                (min, max)
            }
            _ => return Ok(atom),
        };
        self.position += 1;
        if matches!(self.peek(), Some('?' | '+')) {
            return Err("lazy and possessive quantifiers are not supported".into());
        }
        Ok(Node::Repeat { node: Box::new(atom), min, max })
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().ok_or("unexpected end")?;
        self.position += 1;
        Ok(match c {
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let index = if self.chars[self.position..].starts_with(&['?', ':']) {
                    self.position += 2;
                    None
                } else {
                    self.groups += 1;
                    Some(self.groups)
                };
                let inner = self.alternation()?;
                if self.peek() != Some(')') {
                    return Err("unbalanced group".into());
                }
                self.position += 1;
                Node::Group(Box::new(inner), index)
            }
            '[' => {
                let mut ranges = Vec::new();
                loop {
                    let low = self.peek().ok_or("unterminated class")?;
                    self.position += 1;
                    if low == ']' {
                        break;
                    }
                    if low == '^' && ranges.is_empty() {
                        return Err("negated classes are not supported".into());
                    }
                    if self.peek() == Some('-') && self.chars.get(self.position + 1).is_some_and(|&c| c != ']') {
                        let high = self.chars[self.position + 1];
                        self.position += 2;
                        ranges.push((low, high));
                    } else {
                        ranges.push((low, low));
                    }
                }
                Node::Class(ranges)
            }
            '\\' => {
                let escaped = self.peek().ok_or("dangling escape")?;
                self.position += 1;
                match escaped {
                    'd' => Node::Digit,
                    's' => Node::Space,
                    c if c.is_ascii_alphanumeric() => return Err(format!("unsupported escape \\{c}")),
                    c => Node::Literal(c),
                }
            }
            '.' | '*' | '+' | '?' | '{' => return Err(format!("unsupported construct `{c}`")),
            c => Node::Literal(c),
        })
    }
}

/// Shortest round-trip decimal digits and the decimal exponent of the first digit:
/// `value = 0.d1d2d3... * 10^(exponent + 1)`, from Rust's shortest representation.
fn shortest_digits(value: f64) -> (String, i32) {
    shortest_digits_of(&format!("{:e}", value.abs()))
}

fn shortest_digits_of(scientific: &str) -> (String, i32) {
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((scientific, "0"));
    (mantissa.replace('.', ""), exponent.parse().unwrap_or(0))
}

/// `Double.toString` (JDK 19 and later): the shortest decimal that rounds to `value`, or when that
/// has one digit, the closest two-digit decimal that does (`4.9E-324`, not `5.0E-324`).
pub fn java_double_to_string(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0.0");
    }
    let (mut digits, mut exponent) = shortest_digits(value);
    if digits.len() == 1 {
        let two_digits = format!("{:.1e}", value.abs());
        if two_digits.parse::<f64>().ok() == Some(value.abs()) {
            let (closest, closest_exponent) = shortest_digits_of(&two_digits);
            digits = closest.trim_end_matches('0').to_owned();
            if digits.is_empty() {
                digits.push('0');
            }
            exponent = closest_exponent;
        }
    }
    let magnitude = value.abs();
    if (1e-3..1e7).contains(&magnitude) {
        let text = if exponent >= 0 {
            let split = exponent as usize + 1;
            let integer: String = digits.chars().chain(std::iter::repeat('0')).take(split.max(digits.len()).min(split)).collect();
            let fraction = if digits.len() > split { &digits[split..] } else { "0" };
            format!("{integer}.{fraction}")
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        };
        format!("{sign}{text}")
    } else {
        let fraction = if digits.len() > 1 { &digits[1..] } else { "0" };
        format!("{sign}{}.{fraction}E{exponent}", &digits[..1])
    }
}

/// `String.format(Locale.ROOT, "%.<decimals>f", value)`: Java rounds the shortest decimal
/// representation half-up, not the exact binary value.
pub fn java_format_fixed(value: f64, decimals: usize) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let sign = if value.is_sign_negative() { "-" } else { "" };
    let (digits, exponent) = if value == 0.0 { ("0".to_owned(), 0) } else { shortest_digits(value) };
    let point = exponent + 1;
    let mut scaled: Vec<u8> = Vec::new();
    let leading_zeros = (-point).max(0) as usize;
    scaled.extend(std::iter::repeat_n(0u8, leading_zeros));
    scaled.extend(digits.bytes().map(|digit| digit - b'0'));
    let integer_length = point.max(0) as usize;
    if scaled.len() < integer_length {
        scaled.resize(integer_length, 0);
    }
    let point_index = if point > 0 { integer_length } else { 0 };
    let keep = point_index + decimals;
    let round_up = scaled.get(keep).is_some_and(|digit| *digit >= 5);
    scaled.resize(keep, 0);
    if round_up {
        let mut index = keep;
        loop {
            if index == 0 {
                scaled.insert(0, 1);
                break;
            }
            index -= 1;
            if scaled[index] == 9 {
                scaled[index] = 0;
            } else {
                scaled[index] += 1;
                break;
            }
        }
    }
    let integer_digits = scaled.len() - decimals;
    let integer: String = if integer_digits == 0 { "0".into() } else { scaled[..integer_digits].iter().map(|digit| char::from(b'0' + digit)).collect() };
    let fraction: String = scaled[integer_digits..].iter().map(|digit| char::from(b'0' + digit)).collect();
    if decimals == 0 { format!("{sign}{integer}") } else { format!("{sign}{integer}.{fraction}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_doubles_like_java() {
        assert_eq!(parse_java_double(" 48.5d "), Some(48.5));
        assert_eq!(parse_java_double("-.5"), Some(-0.5));
        assert_eq!(parse_java_double("5."), Some(5.0));
        assert_eq!(parse_java_double("1e3f"), Some(1000.0));
        assert_eq!(parse_java_double("0x1p3"), Some(8.0));
        assert_eq!(parse_java_double("0x1.8p1"), Some(3.0));
        assert_eq!(parse_java_double("Infinity"), Some(f64::INFINITY));
        assert!(parse_java_double("NaN").is_some_and(f64::is_nan));
        for refused in ["", ".", "e5", "1e", "inf", "nan", "1,5", "1.5.2", "--1", "0x1", "1 2", "infinity"] {
            assert_eq!(parse_java_double(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn rounds_like_math_round() {
        assert_eq!(java_round(2.5), 3);
        assert_eq!(java_round(-2.5), -2);
        assert_eq!(java_round(0.499_999_999_999_999_94), 0);
        assert_eq!(java_round(f64::NAN), 0);
        assert_eq!(java_round(f64::INFINITY), i64::MAX);
    }

    #[test]
    fn matches_with_backtracking_and_captures() {
        let pattern = JavaPattern::compile(r"^\s*(\d{1,3})\s*(?:°|d| )\s*([0-6]?\d)?\s*$").unwrap();
        let input: Vec<char> = "48 5".chars().collect();
        let captures = pattern.find(&input).unwrap();
        assert_eq!((captures[1], captures[2]), (Some((0, 2)), Some((3, 4))));
        let input: Vec<char> = "48D".chars().collect();
        assert_eq!(pattern.find(&input).unwrap()[2], None);
        assert!(pattern.find(&"1234".chars().collect::<Vec<_>>()).is_none());
        let classes = JavaPattern::compile("^([NSEOW])$").unwrap();
        assert!(classes.find(&['w']).is_some());
    }
}
