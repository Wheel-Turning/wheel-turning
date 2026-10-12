// The References list (Spec 05, Answer assembly, rule 5) is a closed <details> element. A
// footnote marker or a metadata line links to an entry inside it; this script opens the list
// when a link's target is inside it, so the browser can scroll there. Without the script the
// reader opens the list.
(function () {
  function open(id) {
    var target = id.indexOf('ref-') === 0 && document.getElementById(id);
    var list = target && target.closest('details');
    if (list && !list.open) {
      list.open = true;
      return target;
    }
    return null;
  }
  document.addEventListener('click', function (event) {
    var link = event.target.closest && event.target.closest('a[href^="#ref-"]');
    if (link) {
      open(link.getAttribute('href').slice(1));
    }
  });
  window.addEventListener('hashchange', function () {
    var target = open(decodeURIComponent(window.location.hash.slice(1)));
    if (target) {
      target.scrollIntoView();
    }
  });
  var first = open(decodeURIComponent(window.location.hash.slice(1)));
  if (first) {
    first.scrollIntoView();
  }
})();
