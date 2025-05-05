document.addEventListener('DOMContentLoaded', function () {
    const faucetIdElem = document.getElementById('faucetId');
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

    fetchMetadata();

    privateButton.addEventListener('click', () => {handleButtonClick(true)});
    publicButton.addEventListener('click', () => {handleButtonClick(false)});

    function fetchMetadata() {
        fetch(window.location.href + 'get_metadata')
            .then(response => response.json())
            .then(data => {
                faucetIdElem.textContent = data.id;
                for (const amount of data.asset_amount_options){
                    const option = document.createElement('option');
                    option.value = amount;
                    option.textContent = amount;
                    assetSelect.appendChild(option);
                }
            })
            .catch(error => {
                console.error('Error fetching metadata:', error);
                faucetIdElem.textContent = 'Error loading Faucet ID.';
                errorMessage.textContent = 'Failed to load metadata. Please try again.';
                errorMessage.style.display = 'block';
            });
    }

    async function handleButtonClick(isPrivateNote) {
        let accountId = accountIdInput.value.trim();
        errorMessage.style.display = 'none';

        if (!accountId || !/^0x[0-9a-fA-F]{30}$/i.test(accountId)) {
            errorMessage.textContent = !accountId ? "Account ID is required." : "Invalid Account ID.";
            errorMessage.style.display = 'block';
            return;
        }

        privateButton.disabled = true;
        publicButton.disabled = true;

        info.style.display = 'none';
        importCommand.style.display = 'none';

        loading.style.display = 'block';
        try {
            const response = await fetch(window.location.href + 'get_tokens?' + new URLSearchParams({
                account_id: accountId, is_private_note: isPrivateNote, asset_amount: parseInt(assetSelect.value)
            }), {
                    method: "POST"
                });

            if (!response.ok) {
                throw response;
            }

            const blob = await response.blob();
            if(isPrivateNote) {
                importCommand.style.display = 'block';
                downloadBlob(blob, 'note.mno');
            }

            const noteId = response.headers.get('Note-Id');
            noteIdElem.textContent = noteId;
            accountIdElem.textContent = accountId;
            info.style.display = 'block';
        } catch (error) {
            console.error('Error:', error);
            errorMessage.textContent = 'Failed to receive tokens. ' + error.statusText + ' (' + error.status + ')';
            errorMessage.style.display = 'block';
        }
        loading.style.display = 'none';
        privateButton.disabled = false;
        publicButton.disabled = false;
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
