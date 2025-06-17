document.addEventListener('DOMContentLoaded', function () {
    const faucetIdElem = document.getElementById('faucet-id');
    const privateButton = document.getElementById('button-private');
    const publicButton = document.getElementById('button-public');
    const accountAddressInput = document.getElementById('account-address');
    const errorMessage = document.getElementById('error-message');
    const info = document.getElementById('info');
    const importCommand = document.getElementById('import-command');
    const noteIdElem = document.getElementById('note-id');
    const accountIdElem = document.getElementById('command-account-id');
    const assetSelect = document.getElementById('asset-amount');
    const loading = document.getElementById('loading');
    const status = document.getElementById("loading-status");
    const txLink = document.getElementById('tx-link');

    // Check if SHA3 is available right from the start
    if (typeof sha3_256 === 'undefined') {
        console.error("SHA3 library not loaded initially");
        errorMessage.textContent = 'Cryptographic library not loaded. Please refresh the page.';
        errorMessage.style.display = 'block';
    } else {
        console.log("SHA3 library is available at page load");
    }

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
        errorMessage.style.visibility = 'visible';
        info.style.visibility = 'hidden';
    }

    function hideError() {
        errorMessage.style.visibility = 'hidden';
    }

    function validateAccountAddress(accountAddress) {
        if (!accountAddress) {
            showError("Account address is required.");
            return false;
        }

        const isValidFormat = /^(0x[0-9a-fA-F]{30}|[a-z]{1,4}1[a-z0-9]{32})$/i.test(accountAddress);
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
        status.textContent = "";
        info.style.visibility = isLoading ? 'hidden' : 'visible';
        importCommand.style.visibility = isLoading ? 'hidden' : 'visible';
    }

    async function handleButtonClick(isPrivateNote) {
        let accountAddress = accountAddressInput.value.trim();
        hideError();

        if (!validateAccountAddress(accountAddress)) {
            setLoadingState(false);
            return;
        }

        // Check if SHA3 library is loaded
        if (typeof sha3_256 === 'undefined') {
            console.error("SHA3 UNDEFINED when trying to handle button click");
            errorMessage.textContent = "Cryptographic library not loaded. Please refresh the page and try again.";
            errorMessage.style.display = 'block';
            return;
        }

        // Get the PoW challenge from the new /pow endpoint
        let powResponse;
        try {
            powResponse = await fetch(window.location.href + 'pow', {
                method: "GET"
            });
        } catch (error) {
            showError('Connection failed.');
            return;
        }

        if (!powResponse.ok) {
            showError('Please try again soon.');
            return;
        }
        setLoadingState(true);

        status.textContent = "Received Proof of Work challenge";

        const powData = await powResponse.json();

        // Search for a nonce that satisfies the proof of work
        status.textContent = "Solving Proof of Work...";

        const nonce = await findValidNonce(powData.challenge, powData.difficulty);

        // Build query parameters for the request using new challenge format
        const params = {
            account_id: accountAddress,
            is_private_note: isPrivateNote,
            asset_amount: parseInt(assetSelect.value),
            challenge: powData.challenge,
            nonce: nonce
        };


        const evtSource = new EventSource(window.location.href + 'get_tokens?' + new URLSearchParams(params));

        evtSource.onopen = function () {
            status.textContent = "Request on queue...";
        };

        evtSource.onerror = function (_) {
            // Either rate limit exceeded or invalid account id. The error event does not contain the reason.
            evtSource.close();
            setLoadingState(false);
            showError('Please try again soon.');
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
            } else {
                importCommand.style.display = 'none';
            }
            const transactionId = data.transaction_id;
            txLink.href = data.explorer_url + '/tx/' + transactionId;
            txLink.textContent = "waiting for explorer...";

            // Poll the transaction link until it's reachable
            const pollTransaction = async () => {
                try {
                    const response = await fetch(txLink.href);
                    if (response.data) {
                        txLink.textContent = transactionId + " ↗";
                    } else {
                        // If not reachable yet, try again after 1 seconds
                        setTimeout(pollTransaction, 1000);
                    }
                } catch (error) {
                    // If fetch fails, try again after 1 seconds
                    setTimeout(pollTransaction, 1000);
                }
            };

            pollTransaction();
        });
    }

    // Function to find a valid nonce for proof of work using the new challenge format
    async function findValidNonce(challenge, difficulty) {
        // Check again if SHA3 is available
        if (typeof sha3_256 === 'undefined') {
            console.error("SHA3 library not properly loaded. SHA3 object:", sha3_256);
            throw new Error('SHA3 library not properly loaded. Please refresh the page.');
        }

        // Parse difficulty (number of required leading zero BYTES, each byte = 2 hex digits)
        const requiredZeros = parseInt(difficulty);
        const requiredPattern = '00'.repeat(requiredZeros); // Each zero byte = "00" in hex

        let nonce = 0;
        let validNonceFound = false;

        while (!validNonceFound) {
            // Generate a random nonce
            nonce = Math.floor(Math.random() * Number.MAX_SAFE_INTEGER);

            try {
                // Compute hash using SHA3 with the challenge and nonce
                let hash = sha3_256.create();
                hash.update(challenge);  // Use the hex-encoded challenge string directly

                // Convert nonce to 8-byte little-endian format to match backend
                const nonceBytes = new ArrayBuffer(8);
                const nonceView = new DataView(nonceBytes);
                nonceView.setBigUint64(0, BigInt(nonce), true); // true = little-endian
                const nonceByteArray = new Uint8Array(nonceBytes);
                hash.update(nonceByteArray);

                // Get the hex digest
                let digest = hash.hex().toString();

                // Check if the hash starts with the required number of zeros
                if (digest.startsWith(requiredPattern)) {
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
