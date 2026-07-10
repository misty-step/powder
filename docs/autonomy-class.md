# Autonomy Class And Approval Queue

Powder cards carry an `autonomy` class so machine consumers can decide whether
a completed lane is allowed to merge after its oracle, gates, and QA are green,
or whether the lane must wait for an explicit approval answer.

`autonomy` is always serialized on card and card-summary payloads. It is a
decision field, not display metadata.

Valid values:

- `auto`: an external router may autonomously merge the work after the card's
  oracle, repository gates, and live QA proof are green.
- `review`: the conservative default. The work still must be fully verified by
  agents; the operator only supplies the explicit approval answer.

The class can be set when a card is created, revised later through the safe
card patch/update surfaces, or supplied in backlog.d as `Autonomy: auto` or
`Autonomy: review`. GitHub issue imports do not map autonomy yet; they rely on
the conservative `review` default. Powder does not infer class and never calls
a model to classify it.

The Bitterblossom router reads `autonomy` to choose between autonomous merge
and approval-request routing. Other machine consumers should treat omitted
class support as `review`, but current Powder payloads do not omit the field.

## Approval Queue

The approval queue is a read surface over existing awaiting-input primitives.
It does not add lifecycle states or a new approval object. A queue row is an
`awaiting_input` run joined with its card id, title, autonomy class, latest
question text, run id, and packet links.

Packet links are ordinary card links whose `label` starts with `approval`.
`approval/packet` is the preferred label for generated approval packets.

`answer_input` is the green button. Answering the awaiting-input run records
the actor-attributed answer, resumes the run, and removes the row from the
approval queue.

## Coordination Constraints

This contract intentionally does not reshape run or `awaiting_input` machinery;
it only reads through existing run, activity, and link primitives. A later
cleanup may replace that machinery, but this queue must not introduce parallel
state.

This contract also does not add lifecycle states. `auto` and `review` are card
classes, not statuses, so they remain compatible with the three-state direction.
