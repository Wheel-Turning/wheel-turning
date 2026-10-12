// The small-screen menu (Spec 06, Design language, rule 5). The menu is a <details> element and
// works without this script; the script adds Esc to close, a tap on the backdrop to close, a
// Close button, and focus return to the Menu button.
(function () {
  var menus = document.querySelectorAll('details.menu');
  Array.prototype.forEach.call(menus, function (menu) {
    var button = menu.querySelector('summary');
    function close() {
      menu.open = false;
      button.focus();
    }
    Array.prototype.forEach.call(menu.querySelectorAll('[data-close]'), function (el) {
      el.hidden = false;
      el.addEventListener('click', close);
    });
    menu.addEventListener('keydown', function (event) {
      if (event.key === 'Escape' && menu.open) {
        event.preventDefault();
        close();
      }
    });
  });
})();
