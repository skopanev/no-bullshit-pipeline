let _confirmResolve = null;

const confirmModal = document.getElementById('confirm-modal');
const confirmModalTitle = document.getElementById('confirm-modal-title');
const confirmModalMessage = document.getElementById('confirm-modal-message');
const confirmModalCancel = document.getElementById('confirm-modal-cancel');
const confirmModalOk = document.getElementById('confirm-modal-ok');

export function showConfirm(title = 'Are you sure?', message = 'This action cannot be undone.', okLabel = 'Delete') {
  confirmModalTitle.textContent = title;
  confirmModalMessage.textContent = message;
  confirmModalOk.textContent = okLabel;
  confirmModal.style.display = 'flex';
  return new Promise(resolve => { _confirmResolve = resolve; });
}

function _closeConfirm(result) {
  confirmModal.style.display = 'none';
  if (_confirmResolve) { _confirmResolve(result); _confirmResolve = null; }
}

if (confirmModalCancel) confirmModalCancel.addEventListener('click', () => _closeConfirm(false));
if (confirmModalOk) confirmModalOk.addEventListener('click', () => _closeConfirm(true));
if (confirmModal) confirmModal.addEventListener('click', (e) => { if (e.target === confirmModal) _closeConfirm(false); });
