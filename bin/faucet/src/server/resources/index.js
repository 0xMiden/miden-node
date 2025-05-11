document.addEventListener('DOMContentLoaded', function () {
    const faucetIdElem = document.getElementById('faucet-id');
    const privateButton = document.getElementById('button-private');
    const publicButton = document.getElementById('button-public');
    const accountIdInput = document.getElementById('account-id');
    const errorMessage = document.getElementById('error-message');
    const info = document.getElementById('info');
    const importCommand = document.getElementById('import-command');
    const noteIdElem = document.getElementById('note-id');
    const accountIdElem = document.getElementById('command-account-id');
    const assetSelect = document.getElementById('asset-amount');
    const loading = document.getElementById('loading');
    const status = document.getElementById("loading-status");

    fetchMetadata();

    privateButton.addEventListener('click', () => { handleButtonClick(true) });
    publicButton.addEventListener('click', () => { handleButtonClick(false) });

    function fetchMetadata() {
        fetch(window.location.href + 'get_metadata')
            .then(response => response.json())
            .then(data => {
                faucetIdElem.textContent = data.id;
                for (const amount of data.asset_amount_options) {
                    const option = document.createElement('option');
                    option.value = amount;
                    option.textContent = amount;
                    assetSelect.appendChild(option);
                }
            })
            .catch(error => {
                console.error('Error fetching metadata:', error);
                faucetIdElem.textContent = 'Error loading Faucet ID.';
                showError('Failed to load metadata. Please try again.');
            });
    }

    function showError(message) {
        errorMessage.textContent = message;
        errorMessage.style.display = 'block';
    }

    function hideError() {
        errorMessage.style.display = 'none';
    }

    function validateAccountId(accountId) {
        if (!accountId) {
            showError("Account address is required.");
            return false;
        }

        const isValidFormat = /^(0x[0-9a-fA-F]{30}|[a-z]{1,4}1[a-z0-9]{32})$/i.test(accountId);
        if (!isValidFormat) {
            showError("Invalid Account address.");
            return false;
        }

        return true;
    }

    function setLoadingState(isLoading) {
        privateButton.disabled = isLoading;
        publicButton.disabled = isLoading;
        loading.style.display = isLoading ? 'flex' : 'none';
        info.style.display = isLoading ? 'none' : 'block';
        importCommand.style.display = isLoading ? 'none' : 'block';
    }

    async function handleButtonClick(isPrivateNote) {
        let accountId = accountIdInput.value.trim();
        hideError();

        if (!validateAccountId(accountId)) {
            return;
        }

        setLoadingState(true);

        const evtSource = new EventSource(window.location.href + 'get_tokens?' + new URLSearchParams({
            account_id: accountId, is_private_note: isPrivateNote, asset_amount: parseInt(assetSelect.value)
        }));

        evtSource.onopen = function () {
            status.textContent = "Request on queue...";
        };

        evtSource.onerror = function (_) {
            // Either rate limit exceeded or invalid account id. The error event does not contain the reason.
            evtSource.close();
            showError('Failed to receive tokens.');
            setLoadingState(false);
        };

        evtSource.addEventListener("get-tokens-error", function (event) {
            console.error('EventSource failed:', event.data);
            evtSource.close();

            const data = JSON.parse(event.data);
            showError('Failed to receive tokens. ' + data.message);
            setLoadingState(false);
        });

        evtSource.addEventListener("update", function (event) {
            status.textContent = event.data;
        });

        evtSource.addEventListener("note", function (event) {
            evtSource.close();

            let data = JSON.parse(event.data);

            setLoadingState(false);

            noteIdElem.textContent = data.note_id;
            accountIdElem.textContent = data.account_id;
            if (isPrivateNote) {
                importCommand.style.display = 'block';

                // Decode base64
                const binaryString = atob(data.data_base64);
                const byteArray = new Uint8Array(binaryString.length);
                for (let i = 0; i < binaryString.length; i++) {
                    byteArray[i] = binaryString.charCodeAt(i);
                }

                const blob = new Blob([byteArray], { type: 'application/octet-stream' });
                downloadBlob(blob, 'note.mno');
            }
        });
    }

    function downloadBlob(blob, filename) {
        const url = window.URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.style.display = 'none';
        a.href = url;
        a.download = filename;
        document.body.appendChild(a);
        a.click();
        a.remove();
        window.URL.revokeObjectURL(url);
    }
});
