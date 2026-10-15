// Spec 06, Accessibility checks in CI: axe with the WCAG 2.x A and AA tags, 44px targets, reflow
// at 320 CSS pixels (1280 at 400 percent zoom), badges in words, and the progress section while
// a question runs, on every page at 360, 768 and 1280 pixels wide.
const fs = require("fs");
const { test, expect } = require("@playwright/test");
const AxeBuilder = require("@axe-core/playwright").default;

const paths = JSON.parse(fs.readFileSync(process.env.A11Y_PATHS || "paths.json", "utf8"));
const PAGES = [
  ["home", "/"],
  ["answer", paths.answer],
  ["talk", paths.talk],
  ["transcript", paths.transcript],
  ["rights", "/rights"],
  // A well-formed token shows the page with its button; the page reads nothing until it is
  // pressed.
  ["rights-confirm", `/rights/confirm?token=${"A".repeat(43)}`],
  ["about", "/about"],
  ["accessibility", "/accessibility"],
];
const WIDTHS = [360, 768, 1280];
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const BADGE_WORDS = ["resolved", "probable", "unresolved", "verified", "unverified"];

test.beforeEach(async ({ page }) => {
  await page.goto(paths.join);
});

async function axe(page, where) {
  const result = await new AxeBuilder({ page }).withTags(TAGS).analyze();
  const lines = result.violations.flatMap((v) =>
    v.nodes.map((n) => `${where}: ${v.id} (${v.impact}): ${n.target.join(" ")}: ${n.failureSummary}`));
  expect(lines, lines.join("\n")).toEqual([]);
}

// Every control, and every link not inside a sentence, needs a 44 by 44 pixel target. A link
// is inside a sentence when its nearest block holds words besides links; separators and times
// alone do not count.
// A checkbox or radio counts with its label, which also activates it.
async function targets(page, where) {
  const small = await page.evaluate(() => {
    const selector = 'a[href], button, input:not([type="hidden"]), select, textarea, summary';
    const out = [];
    for (const el of document.querySelectorAll(selector)) {
      if (el.closest(".hp, .skip, template")) {
        continue; // the honeypot is never shown, and the skip link is checked when focused
      }
      const details = el.closest("details:not([open])");
      if (details && el.parentElement !== details) {
        continue; // inside a closed menu, so not shown
      }
      const style = getComputedStyle(el);
      let r = el.getBoundingClientRect();
      if (style.visibility === "hidden" || (r.width === 0 && r.height === 0)) {
        continue;
      }
      if (el.tagName === "A") {
        let block = el.parentElement;
        while (block && getComputedStyle(block).display.startsWith("inline")) {
          block = block.parentElement;
        }
        let others = block ? block.textContent : "";
        for (const a of block ? block.querySelectorAll("a") : []) {
          others = others.replace(a.textContent, "");
        }
        if (/[A-Za-z]/.test(others)) {
          continue;
        }
      }
      if ((el.type === "checkbox" || el.type === "radio") && el.labels && el.labels.length) {
        const l = el.labels[0].getBoundingClientRect();
        const left = Math.min(r.left, l.left), top = Math.min(r.top, l.top);
        r = { width: Math.max(r.right, l.right) - left, height: Math.max(r.bottom, l.bottom) - top };
      }
      if (r.width < 44 - 0.5 || r.height < 44 - 0.5) {
        const name = (el.textContent || el.getAttribute("aria-label") || el.name || "").trim().slice(0, 40);
        out.push(`${el.tagName.toLowerCase()} "${name}" is ${Math.round(r.width)}x${Math.round(r.height)}`);
      }
    }
    return out;
  });
  expect(small, `${where}: targets under 44px:\n${small.join("\n")}`).toEqual([]);
}

async function badges(page, where) {
  const words = await page.$$eval(".badge", (els) => els.map((e) => e.textContent.trim().toLowerCase()));
  for (const w of words) {
    expect(BADGE_WORDS, `${where}: badge "${w}"`).toContain(w);
  }
}

for (const [name, path] of PAGES) {
  for (const width of WIDTHS) {
    test(`${name} at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      const res = await page.goto(path);
      expect(res.status()).toBe(200);
      const where = `${name} ${width}px`;
      await axe(page, where);
      await targets(page, where);
      await badges(page, where);
    });
  }

  test(`${name} reflows at 320px (400 percent zoom)`, async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 256 });
    await page.goto(path);
    const sideways = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
    expect(sideways, `${name} scrolls sideways by ${sideways}px`).toBeLessThanOrEqual(0);
  });
}

for (const width of [360, 1280]) {
  test(`the progress section at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/");
    await page.fill("#q", "How do I work with anger?");
    await page.click("form.ask button[type=submit]");
    const heading = page.locator("#progress-h");
    await expect(heading).toBeFocused();
    await expect(page.locator("[data-question]")).toHaveText("How do I work with anger?");
    await expect(page.locator('li[aria-current="step"]')).toHaveCount(1);
    await axe(page, `progress ${width}px`);
    await targets(page, `progress ${width}px`);
    await page.waitForURL(/\/a\//);
    await expect(page.locator("h1.question")).toHaveText("How do I work with anger?");
  });
}

test("the announcements name each stage once", async ({ page }) => {
  const said = [];
  await page.exposeFunction("said", (text) => said.push(text));
  await page.goto("/");
  await page.evaluate(() => {
    // Each record keeps the text it added, so announcements made in one task are all seen.
    new MutationObserver((records) => {
      for (const r of records) {
        if (r.target.matches && r.target.matches("[data-live]")) {
          r.addedNodes.forEach((n) => n.data && window.said(n.data));
        }
      }
    }).observe(document.getElementById("main"), { subtree: true, childList: true });
  });
  await page.fill("#q", "How do I work with anger?");
  await page.click("form.ask button[type=submit]");
  await page.waitForURL(/\/a\//);
  const stages = said.filter((s, i) => s !== said[i - 1]).map((s) => s.split(".")[0]);
  expect(new Set(stages).size, said.join(" | ")).toBe(stages.length);
  expect(stages, said.join(" | ")).toEqual([
    "Reading the question",
    "Searching the talks",
    "Weighing excerpts",
    "Choosing excerpts",
    "Writing the summary",
    "Opening the answer",
  ]);
});

test("without JavaScript a question posts and the answer opens", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(paths.join);
  await page.fill("#q", "How do I work with anger?");
  await page.click("form.ask button[type=submit]");
  await page.waitForURL(/\/a\//);
  await expect(page.locator("h1.question")).toHaveText("How do I work with anger?");
  await context.close();
});

test("return to the top leads to the top of the answer page", async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 700 });
  await page.goto(paths.answer);
  const link = page.getByRole("link", { name: "Return to the top" });
  await link.focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBe(0);
  await page.keyboard.press("Tab");
  await expect(page.getByRole("link", { name: "Skip to main content" })).toBeFocused();
});

test("the menu opens from its button and closes from Close, the backdrop and Esc", async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await page.goto("/about");
  const menu = page.locator("details.menu summary");
  await expect(menu).toHaveText("Menu");
  for (const close of ["button", "backdrop", "Escape"]) {
    await menu.click();
    await expect(page.locator("details.menu")).toHaveAttribute("open", "");
    if (close === "button") {
      await page.getByRole("button", { name: "Close" }).click();
    } else if (close === "backdrop") {
      await page.locator("details.menu .backdrop").click({ position: { x: 10, y: 400 } });
    } else {
      await page.keyboard.press("Escape");
    }
    await expect(page.locator("details.menu")).not.toHaveAttribute("open", "");
    await expect(menu).toBeFocused();
  }
  await page.setViewportSize({ width: 1280, height: 800 });
  await expect(menu).toBeHidden();
  await expect(page.locator("header nav.wide").getByRole("link", { name: "About" })).toBeVisible();
});

test("without JavaScript the menu opens and the dana split shows", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, viewport: { width: 360, height: 800 } });
  const page = await context.newPage();
  await page.goto(paths.join);
  await page.locator("details.menu summary").click();
  await expect(page.locator("details.menu .panel").getByRole("link", { name: "About" })).toBeVisible();
  await page.goto(paths.answer);
  await page.fill("#dana-total", "20");
  await page.getByRole("button", { name: "Show the split" }).click();
  await expect(page.locator("#dana-result")).toContainText("North Fixture Zendo");
  await context.close();
});

test("an acute question shows the notice with focus on its heading, and axe passes", async ({ page }) => {
  await page.setViewportSize({ width: 360, height: 800 });
  await page.goto("/");
  await page.fill("#q", "I want to end my life because of this anger");
  await page.click("form.ask button[type=submit]");
  const heading = page.getByRole("heading", { level: 1, name: "Please reach out for help now" });
  await expect(heading).toBeFocused();
  await expect(page.locator("#dana")).toHaveCount(0);
  await expect(page.locator("form[data-feedback]")).toHaveCount(0);
  await axe(page, "acute 360px");
  await targets(page, "acute 360px");
});
