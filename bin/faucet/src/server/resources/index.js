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

    // Check if SHA3 is available right from the start
    if (typeof sha3_256 === 'undefined') {
        console.error("SHA3 library not loaded initially");
        errorMessage.textContent = 'Cryptographic library not loaded. Please refresh the page.';
        errorMessage.style.display = 'block';
    } else {
        console.log("SHA3 library is available at page load");
    }

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

    privateButton.addEventListener('click', () => {handleButtonClick(true)});
    publicButton.addEventListener('click', () => {handleButtonClick(false)});

    async function handleButtonClick(isPrivateNote) {
        let accountId = accountIdInput.value.trim();
        errorMessage.style.display = 'none';

        if (!accountId || !/^0x[0-9a-fA-F]{30}$/i.test(accountId)) {
            errorMessage.textContent = !accountId ? "Account ID is required." : "Invalid Account ID.";
            errorMessage.style.display = 'block';
            return;
        }

        // Check if SHA3 library is loaded
        if (typeof sha3_256 === 'undefined') {
            console.error("SHA3 UNDEFINED when trying to handle button click");
            errorMessage.textContent = "Cryptographic library not loaded. Please refresh the page and try again.";
            errorMessage.style.display = 'block';
            return;
        }

        privateButton.disabled = true;
        publicButton.disabled = true;

        info.style.display = 'none';
        importCommand.style.display = 'none';

        loading.style.display = 'block';
        try {
            // Get the pow seed, difficulty, and server signature
            const powResponse = await fetch(window.location.href + 'pow', {
                method: "GET"
            });

            const powData = await powResponse.json();

            // Search for a nonce that satisfies the proof of work
            const nonce = await findValidNonce(powData.seed, powData.difficulty);

            const response = await fetch(window.location.href + 'get_tokens?' + new URLSearchParams({
                account_id: accountId,
                is_private_note: isPrivateNote,
                asset_amount: parseInt(assetSelect.value),
                pow_seed: powData.seed,
                pow_solution: nonce,
                server_signature: powData.server_signature,
                server_timestamp: powData.timestamp
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

    // Function to find a valid nonce for proof of work
    async function findValidNonce(seed, difficulty) {
        // Check again if SHA3 is available
        if (typeof sha3_256 === 'undefined') {
            console.error("SHA3 library not properly loaded. SHA3 object:", sha3_256);
            throw new Error('SHA3 library not properly loaded. Please refresh the page.');
        }

        // Parse difficulty (number of required trailing zeros)
        const requiredZeros = parseInt(difficulty);
        const requiredPattern = '0'.repeat(requiredZeros);

        let nonce = 0;
        let validNonceFound = false;

        while (!validNonceFound) {
            // Generate a random nonce
            nonce = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);

            try {
                // Compute hash using SHA3
                let hash = sha3_256.create();
                hash.update(seed);
                hash.update(nonce.toString());
                // Trim leading 0x
                let digest = hash.hex().toString().slice(2);


                // Check if the hash starts with the required number of zeros
                if (digest.startsWith(requiredPattern)) {
                    console.log("Found valid nonce! Nonce:", nonce, "Hash:", digest);
                    validNonceFound = true;
                    return nonce;
                }
            } catch (error) {
                console.error('Error computing hash:', error);
                throw new Error('Failed to compute hash: ' + error.message);
            }

            // Yield to browser to prevent freezing
            if (nonce % 1000 === 0) {
                await new Promise(resolve => setTimeout(resolve, 0));
            }
        }
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
