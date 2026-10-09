// Gate A review page. gate_a.review_html inlines this. Ratings are kept in this browser as the
// reviewer goes and saved as JSON with the Save ratings button.
const RUN = document.body.dataset.run;
const KEY = 'gate-a-ratings-' + RUN;
const form = document.querySelector('form');
const radio = (name, value) =>
  document.querySelector('input[name="' + name + '"]' + (value ? '[value="' + value + '"]' : ':checked'));

function collect() {
  const out = [];
  document.querySelectorAll('.p').forEach((el) => {
    const q = el.dataset.q;
    const p = el.dataset.p;
    const answer = (n) => {
      const c = radio(n + '-' + q + '-' + p);
      return c ? c.value === 'yes' : null;
    };
    out.push({
      question_id: q,
      passage_id: p,
      faithful: answer('f'),
      relevant: answer('r'),
      note: el.querySelector('textarea').value.trim(),
    });
  });
  return out;
}

function count() {
  const r = collect();
  const done = r.filter((x) => x.faithful !== null && x.relevant !== null).length;
  document.getElementById('done').textContent = done + ' of ' + r.length + ' passages rated';
}

function save() {
  try {
    const reviewer = document.getElementById('reviewer').value;
    localStorage.setItem(KEY, JSON.stringify({ reviewer, ratings: collect() }));
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
  for (const r of s.ratings) {
    for (const [n, val] of [['f', r.faithful], ['r', r.relevant]]) {
      if (val === null) continue;
      const c = radio(n + '-' + r.question_id + '-' + r.passage_id, val ? 'yes' : 'no');
      if (c) c.checked = true;
    }
    const sel = '.p[data-q="' + r.question_id + '"][data-p="' + r.passage_id + '"] textarea';
    const t = document.querySelector(sel);
    if (t) t.value = r.note || '';
  }
}

form.addEventListener('change', save);
form.addEventListener('input', save);
document.getElementById('download').addEventListener('click', () => {
  const body = {
    run: RUN,
    reviewer: document.getElementById('reviewer').value.trim(),
    rated_on: new Date().toISOString().slice(0, 10),
    ratings: collect(),
  };
  const blob = new Blob([JSON.stringify(body, null, 2)], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = 'ratings-' + RUN + '.json';
  a.click();
});
restore();
count();
