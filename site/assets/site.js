/* Static content remains usable when scripts or clipboard permissions are unavailable. */
const filters = document.querySelector('.bench-filters');
if (filters) {
  filters.hidden = false;
  filters.addEventListener('click', (event) => {
    const button = event.target.closest('button[data-platform]');
    if (!button) return;
    filters.querySelectorAll('button').forEach((item) => {
      item.setAttribute('aria-pressed', String(item === button));
    });
    document.querySelectorAll('[data-bench-platform]').forEach((panel) => {
      panel.hidden = button.dataset.platform !== 'all' && panel.dataset.benchPlatform !== button.dataset.platform;
    });
  });
}

document.querySelectorAll('[data-copy]').forEach((button) => {
  button.hidden = false;
  button.addEventListener('click', async () => {
    const command = document.getElementById(button.dataset.copy);
    const feedback = document.querySelector('.copy-feedback');
    if (!command || !feedback) return;
    try {
      await navigator.clipboard.writeText(command.textContent.trim());
      feedback.textContent = button.dataset.success;
    } catch {
      const range = document.createRange();
      range.selectNodeContents(command);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      feedback.textContent = button.dataset.failure;
    }
  });
});

/* Hero terminal: replays a scan -- two progress lines redrawn every 20 ms, as
 * the CLI does, then the result. The static markup is the result, which is what
 * reduced-motion and no-script visitors see. Not announced: a region rewritten
 * fifty times a second is noise to a screen reader. */
const demo = document.querySelector('[data-term-demo]');
if (demo && !window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
  const result = demo.textContent;
  const total = 412088;
  const duration = 2600;
  const samples = [
    ['lib.rs', '12.3 KiB'], ['index.js', '4.1 KiB'], ['libstd-8c1e.rlib', '9.80 MiB'],
    ['README.md', '2.7 KiB'], ['package.json', '1.2 KiB'], ['main.o', '388 KiB'],
    ['icon.png', '24.5 KiB'], ['Cargo.lock', '61.0 KiB'], ['bundle.js.map', '1.42 MiB'],
  ];
  const run = () => {
    const start = performance.now();
    let i = 0;
    const tick = () => {
      const elapsed = performance.now() - start;
      if (elapsed >= duration) {
        demo.textContent = result;
        setTimeout(run, 6000);
        return;
      }
      const share = elapsed / duration;
      // Real scans speed up and slow down with the tree; a flat rate reads as fake.
      const files = Math.floor(total * Math.min(1, share + 0.03 * Math.sin(share * 17)));
      const secs = elapsed / 1000;
      const pct = share < 0.15 ? ' --' : String(Math.min(99, Math.floor(share * 100))).padStart(3);
      const [name, size] = samples[i++ % samples.length];
      demo.textContent = `progress: ${pct}% | ${files} files | ${Math.round(files / Math.max(secs, 0.001))} f/s | ${secs.toFixed(1)}s\nscan: ${name} (${size})`;
      setTimeout(tick, 20);
    };
    tick();
  };
  run();
}
