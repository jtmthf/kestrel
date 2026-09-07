# Trigger filters are a dialect, not an expression language

A trigger matches with the CloudEvents Subscriptions filter dialect — `exact`, `prefix`, `suffix`,
`all`, `any`, `not` — implemented in kestrel and extended with path addressing into `data`, which
the spec's dialects do not reach. Briefs and the scalar fields a firing resolves are rendered with
`minijinja` in strict mode.

## Considered options

**An embedded expression language** was the obvious alternative and was rejected on measurement.
`cel` costs 38 transitive dependencies including an ANTLR runtime port, and exposes no cost budget,
timeout or iteration limit — CEL terminates, but a comprehension over a large `data` array is
unbounded work evaluated per event on the control plane, and Kubernetes needed a cost limiter that
the Rust crate has no equivalent of. CESQL is a stable v1.0.0 spec with no Rust implementation at
all, and addresses context attributes only. `jq`/`jaq` embeds cleanly but reads as an alien syntax
in reviewed configuration; `vrl` is 343 dependencies and MPL-2.0; `regorus` is Rego.

The dialect costs zero dependencies, is the only option that maps onto SQL prefilters (`exact` to
`=`, `prefix` to `LIKE 'x%'`), and can be printed back to a human, which is what makes
`kestrel trigger list` and `kestrel trigger test` mean anything. Adding a language later is a new
key in the filter object; removing one breaks every declaration in the field.

`minijinja` was chosen over `tera` 2 and `handlebars` for two reasons that outweigh Tera's
errors-by-default: it carries four transitive dependencies, and it is the only engine in the field
with runtime resource limits — fuel, a recursion cap, and a maximum output — which matter because
templates are operator-authored config evaluated inside the control plane. Its strict-mode error
names the missing field with a span and dumps the variables in scope, which is what a failing
`trigger test` needs to be useful.

## Consequences

- **The `data` extension is kestrel's, not the spec's.** A filter that reaches into `data` is not
  portable to another CloudEvents implementation, and should not be described as conformant.
- **Undefined is an error, not the empty string.** A brief that renders `on branch ` because the
  payload had no `pull_request` would start a run that burns money discovering it has nothing to
  work with. Strictness is only tolerable alongside `kestrel trigger test`, which is why unmatched
  events are retained: without a recorded event there is nothing to dry-run against.
- **The first filter nobody can express is the argument for a language**, and evidence beats
  anticipation.
