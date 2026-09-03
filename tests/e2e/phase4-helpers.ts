import { expect, type APIResponse, type Locator, type Page, type TestInfo } from '@playwright/test';

export const PHASE4_BROWSER_PROJECTS = new Set([
  'chromium',
  'webkit',
  'firefox',
  'mobile-webkit',
  'firefox-nojs',
]);

export const BUILTIN_THEMES = [
  ['forest', 'Forest'],
  ['blue-sky', 'Blue Sky'],
  ['deep-orbit', 'Deep Orbit'],
  ['terminal', 'Terminal'],
  ['dorfic', 'DORFic'],
  ['chanclassic', 'ChanClassic'],
  ['aero', 'Frutiger Aero'],
  ['neoncubicle', 'NeonCubicle'],
  ['fluorogrid', 'FluoroGrid'],
] as const;

const UNSAFE_BODY_PATTERNS = [
  /thread panicked/i,
  /stack backtrace/i,
  /SQLITE_/i,
  /database is locked/i,
  /cookie_secret/i,
  /password_hash/i,
  /\/Users\//i,
  /\/var\/folders\//i,
  /\/tmp\/rustchan/i,
  /target\/debug/i,
  /rustchan-data/i,
  /SECRET_SENTINEL_DO_NOT_LEAK/i,
];

export function phase4SkipUnless(testInfo: TestInfo, projects: string[], reason: string): void {
  testInfo.skip(!projects.includes(testInfo.project.name), reason);
}

export function isMobileProject(testInfo: TestInfo): boolean {
  return testInfo.project.name.includes('mobile');
}

export async function expectSafeText(text: string, label: string): Promise<void> {
  for (const pattern of UNSAFE_BODY_PATTERNS) {
    expect(text, `${label} should not leak ${pattern}`).not.toMatch(pattern);
  }
}

export async function expectSafeBody(page: Page, label = 'page'): Promise<void> {
  await expect(page.locator('body'), `${label} body should render`).toBeVisible();
  await expectSafeText(await page.locator('body').innerText(), label);
}

export async function expectSafeHtmlResponse(response: APIResponse, label: string): Promise<string> {
  const text = await response.text();
  await expectSafeText(text, label);
  return text;
}

export async function expectSafeHeaders(response: APIResponse, label: string): Promise<void> {
  const headers = response.headers();
  expect(headers['x-content-type-options'], `${label} should use nosniff`).toBe('nosniff');
  expect(headers['referrer-policy'], `${label} should limit referrers`).toBe('same-origin');
  expect(headers['x-frame-options'], `${label} should frame-protect pages`).toBe('SAMEORIGIN');
  expect(headers['permissions-policy'], `${label} should restrict device permissions`).toContain('camera=()');
  expect(headers['content-security-policy'], `${label} should emit CSP`).toContain("frame-ancestors 'none'");
}

export function watchClientErrors(page: Page): () => void {
  const errors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') {
      errors.push(message.text());
    }
  });
  page.on('pageerror', (error) => {
    errors.push(error.message);
  });
  return () => {
    const unexpected = errors.filter((message) => !/ResizeObserver loop/i.test(message)
      && !/Failed to load resource: the server responded with a status of 4(?:00|03|04|10|13|15|22)/i.test(message));
    expect(unexpected).toEqual([]);
  };
}

export async function expectNoHorizontalOverflow(page: Page, label: string, tolerance = 8): Promise<void> {
  const overflow = await page.evaluate(() => {
    const doc = document.documentElement;
    const body = document.body;
    return Math.max(doc.scrollWidth - doc.clientWidth, body.scrollWidth - body.clientWidth);
  });
  expect(overflow, `${label} should not create unusable horizontal scrolling`).toBeLessThanOrEqual(tolerance);
}

export async function expectFocusVisible(locator: Locator, label: string): Promise<void> {
  await locator.focus();
  const focus = await locator.evaluate((element) => {
    const style = window.getComputedStyle(element);
    return {
      outlineStyle: style.outlineStyle,
      outlineWidth: Number.parseFloat(style.outlineWidth || '0'),
      boxShadow: style.boxShadow,
      borderColor: style.borderColor,
    };
  });
  expect(
    (focus.outlineStyle !== 'none' && focus.outlineWidth > 0)
      || focus.boxShadow !== 'none'
      || focus.borderColor !== '',
    `${label} should expose a visible focus affordance`,
  ).toBeTruthy();
}

export async function expectUsableTarget(locator: Locator, label: string, testInfo: TestInfo): Promise<void> {
  await expect(locator, `${label} should be visible`).toBeVisible();
  const box = await locator.boundingBox();
  expect(box, `${label} should have layout bounds`).not.toBeNull();
  expect(box!.width, `${label} should not collapse horizontally`).toBeGreaterThan(0);
  expect(box!.height, `${label} should be large enough to target`).toBeGreaterThanOrEqual(
    isMobileProject(testInfo) ? 36 : 24,
  );
}

export async function expectNoCoveredCenters(locator: Locator, label: string): Promise<void> {
  const covered = await locator.evaluateAll((elements) => elements
    .filter((element) => {
      const rect = element.getBoundingClientRect();
      const style = window.getComputedStyle(element);
      return rect.width > 0 && rect.height > 0 && style.visibility !== 'hidden' && style.display !== 'none';
    })
    .map((element, index) => {
      const rect = element.getBoundingClientRect();
      const points = [
        [0.5, 0.5],
        [0.3, 0.5],
        [0.7, 0.5],
        [0.5, 0.3],
        [0.5, 0.7],
      ];
      let hasPointInViewport = false;
      const hasReachablePoint = points.some(([xRatio, yRatio]) => {
        const x = rect.left + rect.width * xRatio;
        const y = rect.top + rect.height * yRatio;
        if (x < 0 || y < 0 || x > window.innerWidth || y > window.innerHeight) {
          return false;
        }
        hasPointInViewport = true;
        const top = document.elementFromPoint(x, y);
        return top !== null && (element === top || element.contains(top) || top.contains(element));
      });
      if (!hasPointInViewport) {
        return null;
      }
      return hasReachablePoint ? null : index + 1;
    })
    .filter((index): index is number => index !== null));
  expect(covered, `${label} controls should expose a reachable hit point`).toEqual([]);
}

export async function expectNamedInteractiveControls(page: Page, rootSelector: string, label: string): Promise<void> {
  const missing = await page.locator(rootSelector).evaluate((root) => {
    function isVisible(element: Element): boolean {
      const style = window.getComputedStyle(element);
      const rect = element.getBoundingClientRect();
      return style.display !== 'none' && style.visibility !== 'hidden' && rect.width > 0 && rect.height > 0;
    }

    function textOf(id: string): string {
      return (document.getElementById(id)?.textContent || '').trim();
    }

    function associatedLabelText(control: Element): string {
      if (control instanceof HTMLInputElement || control instanceof HTMLTextAreaElement || control instanceof HTMLSelectElement) {
        const labels = Array.from(control.labels || []);
        return labels.map((item) => item.textContent || '').join(' ').trim();
      }
      return '';
    }

    function accessibleName(control: Element): string {
      const aria = control.getAttribute('aria-label') || '';
      const labelledBy = (control.getAttribute('aria-labelledby') || '')
        .split(/\s+/)
        .map(textOf)
        .join(' ')
        .trim();
      const label = associatedLabelText(control);
      const title = control.getAttribute('title') || '';
      const placeholder = control.getAttribute('placeholder') || '';
      const value = control instanceof HTMLInputElement && ['submit', 'button', 'reset'].includes(control.type)
        ? control.value
        : '';
      const imageAlt = Array.from(control.querySelectorAll('img[alt]'))
        .map((image) => image.getAttribute('alt') || '')
        .join(' ')
        .trim();
      const text = control.textContent || '';
      return [aria, labelledBy, label, title, placeholder, value, imageAlt, text].join(' ').trim();
    }

    return Array.from(root.querySelectorAll('a[href], button, input, select, textarea, summary'))
      .filter((element) => {
        if (!isVisible(element)) return false;
        if (element instanceof HTMLInputElement && element.type === 'hidden') return false;
        return true;
      })
      .filter((element) => accessibleName(element).length === 0)
      .map((element) => {
        const id = element.getAttribute('id');
        const name = element.getAttribute('name');
        const type = element instanceof HTMLInputElement ? element.type : element.tagName.toLowerCase();
        return [type, id ? `#${id}` : '', name ? `[name="${name}"]` : ''].filter(Boolean).join('');
      });
  });
  expect(missing, `${label} should not expose unnamed interactive controls`).toEqual([]);
}

export async function expectReadableContrast(page: Page, selector: string, label: string, minimum = 3): Promise<void> {
  const ratio = await page.locator(selector).first().evaluate((element) => {
    function parseRgb(value: string): [number, number, number, number] | null {
      const match = value.match(/rgba?\(([^)]+)\)/);
      if (!match) return null;
      const parts = match[1].split(',').map((part) => Number.parseFloat(part.trim()));
      if (parts.length < 3) return null;
      return [parts[0], parts[1], parts[2], parts.length > 3 ? parts[3] : 1];
    }

    function relativeLuminance([r, g, b]: [number, number, number]): number {
      const channel = (value: number) => {
        const scaled = value / 255;
        return scaled <= 0.03928 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
      };
      return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
    }

    function contrast(fg: [number, number, number], bg: [number, number, number]): number {
      const lighter = Math.max(relativeLuminance(fg), relativeLuminance(bg));
      const darker = Math.min(relativeLuminance(fg), relativeLuminance(bg));
      return (lighter + 0.05) / (darker + 0.05);
    }

    function composite(top: [number, number, number, number], bottom: [number, number, number]): [number, number, number] {
      const alpha = top[3];
      return [
        top[0] * alpha + bottom[0] * (1 - alpha),
        top[1] * alpha + bottom[1] * (1 - alpha),
        top[2] * alpha + bottom[2] * (1 - alpha),
      ];
    }

    const foreground = parseRgb(window.getComputedStyle(element).color);
    const layers: Array<[number, number, number, number]> = [];
    let current: Element | null = element;
    while (current) {
      const parsed = parseRgb(window.getComputedStyle(current).backgroundColor);
      if (parsed && parsed[3] > 0) {
        layers.push(parsed);
      }
      current = current.parentElement;
    }
    let background: [number, number, number] = [255, 255, 255];
    for (const layer of layers.reverse()) {
      background = composite(layer, background);
    }
    if (!foreground) return 0;
    return contrast([foreground[0], foreground[1], foreground[2]], background);
  });
  expect(ratio, `${label} contrast ratio`).toBeGreaterThanOrEqual(minimum);
}
