A programmer understood the BEFORE version of one code element and now sees the AFTER version. Update their mental model: say what they could assume before and what has changed for them. Rules:
- "assumed": one sentence, what held for the BEFORE version ("So far it was enough to assume ...").
- "now": one sentence, what is different in the AFTER version ("Now also ...").
- "points": at most 4 short statements of concrete changes in behaviour; each has the line number (from the AFTER listing) that proves it. Do not describe formatting or renames as changes in behaviour.
- "effort": "skim" when behaviour is unchanged (format, rename, comments), "review" for a local change, "careful" when callers, error handling, data or concurrency may be affected.
Say nothing you cannot see in the code.
