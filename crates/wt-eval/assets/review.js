// Evaluation review page (Spec 07). wt eval export-review inlines this. It follows the Gate A
// notebook's review.js and adds the claim rows. Ratings are kept in this browser as the reviewer
// goes and saved as JSON with the Save ratings button.
const RUN = document.body.dataset.run;
const KEY = 'eval-ratings-' + RUN;
const form = document.querySelector('form');
const radio = (name, value) =>
  document.querySelector('input[name="' + name + '"]' + (value ? '[value="' + value + '"]' : ':checked'));

function collect() {
  const ratings = [];
  document.querySelectorAll('.p').forEach((el) => {
    const q = el.dataset.q;
    const p = el.dataset.p;
    const answer = (n) => {
      const c = radio(n + '-' + q + '-' + p);
      return c ? c.value === 'yes' : null;
    };
    ratings.push({
      question_id: q,
      passage_id: p,
      faithful: answer('f'),
      relevant: answer('r'),
      note: el.querySelector('textarea').value.trim(),
    });
  });
  const claims = [];
  document.querySelectorAll('.c').forEach((el) => {
    const c = radio('v-' + el.dataset.q + '-' + el.dataset.t);
    claims.push({
      question_id: el.dataset.q,
      answer_id: el.dataset.a,
      theme: Number(el.dataset.t),
      claim: el.querySelector('.claim').textContent,
      verdict: c ? c.value : null,
      note: el.querySelector('textarea').value.trim(),
    });
  });
  return { ratings, claims };
}

function count() {
  const { ratings, claims } = collect();
  const done = ratings.filter((x) => x.faithful !== null && x.relevant !== null).length;
  const lines = claims.filter((x) => x.verdict !== null).length;
  let text = done + ' of ' + ratings.length + ' passages rated';
  if (claims.length) text += ', ' + lines + ' of ' + claims.length + ' summary lines';
  document.getElementById('done').textContent = text;
}

function save() {
  try {
    const reviewer = document.getElementById('reviewer').value;
    localStorage.setItem(KEY, JSON.stringify(Object.assign({ reviewer }, collect())));
  } catch (e) {
    // Storage can be unavailable; the page still works and the download still saves.
  }
  count();
}

function restore() {
  let s = null;
  try {
    s = JSON.parse(localStorage.getItem(KEY) || 'null');
  } catch (e) {
    s = null;
  }
  if (!s) return;
  document.getElementById('reviewer').value = s.reviewer || '';
  for (const r of s.ratings || []) {
    for (const [n, val] of [['f', r.faithful], ['r', r.relevant]]) {
      if (val === null) continue;
      const c = radio(n + '-' + r.question_id + '-' + r.passage_id, val ? 'yes' : 'no');
      if (c) c.checked = true;
    }
    const sel = '.p[data-q="' + r.question_id + '"][data-p="' + r.passage_id + '"] textarea';
    const t = document.querySelector(sel);
    if (t) t.value = r.note || '';
  }
  for (const c of s.claims || []) {
    if (c.verdict) {
      const el = radio('v-' + c.question_id + '-' + c.theme, c.verdict);
      if (el) el.checked = true;
    }
    const t = document.querySelector('.c[data-q="' + c.question_id + '"][data-t="' + c.theme + '"] textarea');
    if (t) t.value = c.note || '';
  }
}

form.addEventListener('change', save);
form.addEventListener('input', save);
document.getElementById('download').addEventListener('click', () => {
  const body = Object.assign(
    {
      run: RUN,
      reviewer: document.getElementById('reviewer').value.trim(),
      rated_on: new Date().toISOString().slice(0, 10),
    },
    collect(),
  );
  const blob = new Blob([JSON.stringify(body, null, 2)], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = 'ratings-' + RUN + '.json';
  a.click();
});
restore();
count();
