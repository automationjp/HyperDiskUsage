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
