# task_004 (task_1)

## Instruction (verbatim)

During the specified time range of March 4, 2021, from 19:00 to 19:30, the system
experienced one failure. The exact time of the root cause occurrence is currently
unknown. Please pinpoint the root cause occurrence datetime.

## Asked for

- root cause occurrence datetime

(component and reason not requested — but they still have to be identified to date
the onset correctly)

## Window

Stated: 2021-03-04 19:00:00 – 19:30:00 (UTC+8, `ts`).
Materialised with margin: 2021-03-04 18:00:00 – 21:00:00.

Note: task_002 placed a Redis02 high-memory fault at 18:09–18:10 the same day, so
the left margin deliberately starts after 18:00 and that event must not be mistaken
for this one.

## Notes
