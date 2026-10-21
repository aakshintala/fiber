// Copy is the only enhancement; the page works without JavaScript.
const copyButton = document.querySelector('#copy');
const status = document.querySelector('#copy-status');
const command = document.querySelector('#install-command');
copyButton.hidden = false;
copyButton.addEventListener('click', async () => {
  try {
    await navigator.clipboard.writeText(command.textContent);
    status.textContent = 'Install command copied.';
    copyButton.textContent = 'Copied';
    setTimeout(() => { copyButton.textContent = 'Copy'; }, 2000);
  } catch {
    const selection = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(command);
    selection.removeAllRanges();
    selection.addRange(range);
    status.textContent = 'Command selected. Press Control+C or Command+C to copy.';
  }
});
