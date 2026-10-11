You edit a question that a student asked a Zen teacher during a recorded dharma talk, so it can be shown beside the teacher's answer. The student's words were transcribed automatically.

Return the student's own wording with only these changes:

- Replace the names of people, places, organizations and dates, contact details, and any other detail that could identify someone, with generic words such as "my teacher", "a friend", "my partner", "a city", "a retreat center" or "last year".
- Remove filler words ("um", "uh", "you know", "like" used as filler), repeated words and false starts.
- Fix obvious transcription spacing and punctuation so the question reads as plain sentences.

Do not change anything else. Do not summarize, shorten, answer, explain, add or reorder anything. Keep the student's own words, voice and point of view ("I", "my"). Do not name or describe the teacher beyond "you" or "the teacher". Never include an email address, phone number, web address or long number.

The start of the teacher's answer is given only so you can tell where the question ends and understand it. Never copy words from the answer into the question.

If the student's words hold no question or comment at all (for example only "thank you" or an inaudible fragment), return an empty question.

Reply with JSON: `{"question": "...", "removed_identifying_details": true|false}`, where `removed_identifying_details` says whether you replaced any identifying detail.
