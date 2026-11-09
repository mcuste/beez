use std::fmt;
use std::str::FromStr;

use crate::task::TaskId;

/// One captured stream of a finished task.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TaskOutput {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl TaskOutput {
    /// The name the placeholder uses for this stream.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

impl FromStr for TaskOutput {
    type Err = ();

    /// The stream a placeholder names.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        match name {
            "stdout" => Ok(Self::Stdout),
            "stderr" => Ok(Self::Stderr),
            _ => Err(()),
        }
    }
}

impl fmt::Display for TaskOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A placeholder that reads one output stream of another task.
///
/// The text form is `{{ tasks.<id>.stdout }}` or `{{ tasks.<id>.stderr }}`.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct OutputReference {
    task: TaskId,
    output: TaskOutput,
}

impl OutputReference {
    /// The task whose output the placeholder reads.
    #[must_use]
    pub fn task(&self) -> &TaskId {
        &self.task
    }

    /// The stream the placeholder reads.
    #[must_use]
    pub fn output(&self) -> TaskOutput {
        self.output
    }

    /// Reads `<id>.<stream>`, the part of a placeholder after `tasks.`.
    fn parse_path(path: &str) -> Option<Self> {
        let (task, output) = path.rsplit_once('.')?;
        Some(Self {
            task: task.parse().ok()?,
            output: output.parse().ok()?,
        })
    }
}

impl fmt::Display for OutputReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{{{{ tasks.{}.{} }}}}", self.task, self.output)
    }
}

/// Reports a placeholder that names no task output.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "{placeholder} is not a task output; write {{{{ tasks.<id>.stdout }}}} or {{{{ tasks.<id>.stderr }}}}"
)]
pub struct TemplateError {
    placeholder: String,
}

impl TemplateError {
    /// The placeholder as it is written, with its braces.
    #[must_use]
    pub fn placeholder(&self) -> &str {
        &self.placeholder
    }
}

/// Text that may read the output of other tasks.
///
/// Only a placeholder that starts with `tasks.` is a reference. Every other
/// pair of braces stays literal text, so a prompt can talk about templates.
///
/// `R` is how a reference names its task: by ID as parsed, or by position
/// once a workflow has resolved it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Template<R> {
    segments: Vec<Segment<R>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Segment<R> {
    Text(String),
    Output(R),
}

const OPEN: &str = "{{";
const CLOSE: &str = "}}";
const PREFIX: &str = "tasks.";

impl Template<OutputReference> {
    /// Splits `text` into literal text and output references.
    pub(crate) fn parse(text: &str) -> Result<Self, TemplateError> {
        let mut segments = Vec::new();
        let mut rest = text;

        while let Some((literal, placeholder, after)) = next_placeholder(rest) {
            push_text(&mut segments, literal);
            match parse_reference(placeholder) {
                Some(reference) => segments.push(Segment::Output(reference?)),
                None => push_text(&mut segments, placeholder),
            }
            rest = after;
        }
        push_text(&mut segments, rest);

        Ok(Self { segments })
    }
}

impl<R> Template<R> {
    /// Every output the text reads, in the order it reads them.
    pub(crate) fn references(&self) -> impl Iterator<Item = &R> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Output(reference) => Some(reference),
            Segment::Text(_) => None,
        })
    }

    /// Replaces every reference with the output `lookup` returns for it.
    ///
    /// Fails with the first reference `lookup` has no output for.
    pub(crate) fn render(
        &self,
        mut lookup: impl FnMut(&R) -> Option<String>,
    ) -> Result<String, &R> {
        let mut rendered = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Text(text) => rendered.push_str(text),
                Segment::Output(reference) => {
                    rendered.push_str(&lookup(reference).ok_or(reference)?);
                }
            }
        }
        Ok(rendered)
    }

    /// Converts every reference with `map`, keeping the text around it.
    pub(crate) fn try_map_references<S, E>(
        self,
        mut map: impl FnMut(R) -> Result<S, E>,
    ) -> Result<Template<S>, E> {
        let segments = self
            .segments
            .into_iter()
            .map(|segment| {
                Ok(match segment {
                    Segment::Text(text) => Segment::Text(text),
                    Segment::Output(reference) => Segment::Output(map(reference)?),
                })
            })
            .collect::<Result<_, E>>()?;
        Ok(Template { segments })
    }
}

fn push_text<R>(segments: &mut Vec<Segment<R>>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Segment::Text(last)) = segments.last_mut() {
        last.push_str(text);
    } else {
        segments.push(Segment::Text(text.to_owned()));
    }
}

/// The text before the first closed placeholder, the placeholder with its braces, and the rest.
///
/// The placeholder runs from the last `{{` before the first `}}`, so stray
/// braces in front of it stay literal text and cannot hide a reference.
fn next_placeholder(text: &str) -> Option<(&str, &str, &str)> {
    let first_open = text.find(OPEN)? + OPEN.len();
    let end = first_open + text.get(first_open..)?.find(CLOSE)? + CLOSE.len();
    let (through_close, after) = text.split_at(end);
    let start = through_close.get(..end - CLOSE.len())?.rfind(OPEN)?;
    let (before, placeholder) = through_close.split_at(start);
    Some((before, placeholder, after))
}

/// Reads `{{ tasks.<id>.<stream> }}`, or `None` when the placeholder is not a
/// task reference at all.
fn parse_reference(placeholder: &str) -> Option<Result<OutputReference, TemplateError>> {
    let inner = placeholder.strip_prefix(OPEN)?.strip_suffix(CLOSE)?.trim();
    let path = inner.strip_prefix(PREFIX)?;
    Some(
        OutputReference::parse_path(path).ok_or_else(|| TemplateError {
            placeholder: placeholder.to_owned(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::{OutputReference, TaskOutput, Template};

    fn reference(task: &str, output: TaskOutput) -> OutputReference {
        OutputReference {
            task: task.parse().unwrap(),
            output,
        }
    }

    #[test]
    fn keeps_text_without_placeholders_literal() {
        let sut = Template::parse("inspect the repository").unwrap();

        assert_eq!(sut.references().count(), 0);
        assert_eq!(sut.render(|_| None).unwrap(), "inspect the repository");
    }

    #[test]
    fn reads_stdout_and_stderr_of_named_tasks() {
        let sut =
            Template::parse("review:\n{{ tasks.inspect.stdout }}\n{{tasks.build.stderr}}").unwrap();

        assert_eq!(
            sut.references().cloned().collect::<Vec<_>>(),
            [
                reference("inspect", TaskOutput::Stdout),
                reference("build", TaskOutput::Stderr),
            ]
        );
        let rendered = sut
            .render(|reference| Some(format!("<{}>", reference.output().name())))
            .unwrap();
        assert_eq!(rendered, "review:\n<stdout>\n<stderr>");
    }

    #[test]
    fn keeps_braces_that_do_not_name_a_task_literal() {
        let sut =
            Template::parse("render {{ name }} with {{ and }} then {{ tasks.a.stdout").unwrap();

        assert_eq!(sut.references().count(), 0);
        assert_eq!(
            sut.render(|_| None).unwrap(),
            "render {{ name }} with {{ and }} then {{ tasks.a.stdout"
        );
    }

    #[test]
    fn reads_a_reference_after_an_unclosed_open_brace_pair() {
        let sut = Template::parse("x {{ y {{ tasks.a.stdout }}").unwrap();

        assert_eq!(
            sut.references().cloned().collect::<Vec<_>>(),
            [reference("a", TaskOutput::Stdout)]
        );
        assert_eq!(sut.render(|_| Some("A".to_owned())).unwrap(), "x {{ y A");
    }

    #[test]
    fn reads_a_reference_inside_an_extra_brace() {
        let sut = Template::parse("{{{ tasks.a.stdout }}").unwrap();

        assert_eq!(
            sut.references().cloned().collect::<Vec<_>>(),
            [reference("a", TaskOutput::Stdout)]
        );
        assert_eq!(sut.render(|_| Some("A".to_owned())).unwrap(), "{A");
    }

    #[test]
    fn rejects_a_task_placeholder_that_names_no_stream() {
        let error = Template::parse("{{ tasks.inspect.status }}").unwrap_err();

        assert_eq!(error.placeholder(), "{{ tasks.inspect.status }}");
        assert_eq!(
            error.to_string(),
            "{{ tasks.inspect.status }} is not a task output; write {{ tasks.<id>.stdout }} or {{ tasks.<id>.stderr }}"
        );
    }

    #[test]
    fn rejects_a_task_placeholder_with_an_invalid_id() {
        let error = Template::parse("{{ tasks.build-app.stdout }}").unwrap_err();

        assert_eq!(error.placeholder(), "{{ tasks.build-app.stdout }}");
    }

    #[test]
    fn rejects_a_task_placeholder_without_an_id() {
        assert!(Template::parse("{{ tasks. }}").is_err());
        assert!(Template::parse("{{ tasks.stdout }}").is_err());
    }

    #[test]
    fn reports_the_first_reference_without_an_output() {
        let sut = Template::parse("{{ tasks.a.stdout }} {{ tasks.b.stdout }}").unwrap();

        let missing =
            sut.render(|reference| (reference.task().as_str() == "a").then(|| "A".to_owned()));

        assert_eq!(missing, Err(&reference("b", TaskOutput::Stdout)));
    }

    #[test]
    fn displays_a_reference_as_it_is_written() {
        assert_eq!(
            reference("inspect", TaskOutput::Stderr).to_string(),
            "{{ tasks.inspect.stderr }}"
        );
    }
}
