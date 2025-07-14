#![allow(clippy::similar_names, reason = "naming dummy test values is hard")]
#![allow(clippy::too_many_lines, reason = "test code can be long")]

use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use diesel::{Connection, SqliteConnection};
use miden_lib::{
    account::auth::AuthRpoFalcon512, note::create_p2id_note, transaction::TransactionKernel,
};
use miden_lib::{note::create_p2id_note, transaction::TransactionKernel};
use miden_node_proto::domain::account::AccountSummary;
use miden_objects::{
    EMPTY_WORD, Felt, FieldElement, Word, ZERO,
    account::{
        Account, AccountBuilder, AccountComponent, AccountDelta, AccountId, AccountIdVersion,
        AccountStorageDelta, AccountStorageMode, AccountType, AccountVaultDelta, StorageSlot,
        delta::AccountUpdateDetails,
    },
    asset::{Asset, FungibleAsset, NonFungibleAsset, NonFungibleAssetDetails},
    block::{BlockAccountUpdate, BlockHeader, BlockNoteIndex, BlockNoteTree, BlockNumber},
    crypto::{dsa::rpo_falcon512::PublicKey, merkle::SparseMerklePath, rand::RpoRandomCoin},
    note::{
        Note, NoteDetails, NoteExecutionHint, NoteId, NoteMetadata, NoteTag, NoteType, Nullifier,
    },
    testing::account_id::{
        ACCOUNT_ID_PRIVATE_SENDER, ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET,
        ACCOUNT_ID_PUBLIC_NON_FUNGIBLE_FAUCET, ACCOUNT_ID_REGULAR_PRIVATE_ACCOUNT_UPDATABLE_CODE,
    },
    transaction::{OrderedTransactionHeaders, TransactionHeader, TransactionId},
};
use pretty_assertions::assert_eq;
use rand::Rng;

use super::{AccountInfo, NoteRecord, NullifierInfo};
use crate::{
    db::{
        TransactionSummary,
        migrations::apply_migrations,
        models::{Page, queries, utils},
    },
    errors::DatabaseError,
};

fn create_db() -> SqliteConnection {
    let mut conn = SqliteConnection::establish(":memory:").expect("In memory sqlite always works");
    apply_migrations(&mut conn).expect("Migrations always work on an empty database");
    conn
}

fn create_block(conn: &mut SqliteConnection, block_num: BlockNumber) {
    let block_header = BlockHeader::new(
        1_u8.into(),
        num_to_word(2),
        block_num,
        num_to_word(4),
        num_to_word(5),
        num_to_word(6),
        num_to_word(7),
        num_to_word(8),
        num_to_word(9),
        num_to_word(10),
        11_u8.into(),
    );

    conn.transaction(|conn| queries::insert_block_header(conn, &block_header))
        .unwrap();
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_insert_nullifiers_for_block() {
    let mut conn = create_db();
    let conn = &mut conn;
    let nullifiers = [num_to_nullifier(1 << 48)];

    let block_num = 1.into();
    create_block(conn, block_num);

    // Insert a new nullifier succeeds
    {
        conn.transaction(|conn| {
            let res = queries::insert_nullifiers_for_block(conn, &nullifiers, block_num);
            assert_eq!(res.unwrap(), nullifiers.len(), "There should be one entry");
            Ok::<_, DatabaseError>(())
        })
        .unwrap();
    }

    // Inserting the nullifier twice is an error
    {
        let res = queries::insert_nullifiers_for_block(conn, &nullifiers, block_num);
        assert!(res.is_err(), "Inserting the same nullifier twice is an error");
    }

    // even if the block number is different
    {
        let res = queries::insert_nullifiers_for_block(conn, &nullifiers, block_num + 1);

        assert!(
            res.is_err(),
            "Inserting the same nullifier twice is an error, even if with a different block number"
        );
    }

    // test inserting multiple nullifiers
    {
        let nullifiers: Vec<_> = (0..10).map(num_to_nullifier).collect();
        let block_num = 1.into();

        let res = queries::insert_nullifiers_for_block(conn, &nullifiers, block_num);

        assert_eq!(res.unwrap(), nullifiers.len(), "There should be 10 entries");
    }
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_insert_transactions() {
    let mut conn = create_db();
    let conn = &mut conn;
    let count = insert_transactions(conn);

    assert_eq!(count, 2, "Two elements must have been inserted");
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_select_transactions() {
    fn query_transactions(conn: &mut SqliteConnection) -> Vec<TransactionSummary> {
        queries::select_transactions_by_accounts_and_block_range(
            conn,
            &[AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap()],
            // TODO use a Range<BlockNumber> expression
            0.into(),
            2.into(),
        )
        .unwrap()
    }

    let mut conn = create_db();
    let conn = &mut conn;
    let transactions = query_transactions(conn);

    assert!(transactions.is_empty(), "No elements must be initially in the DB");

    let count = insert_transactions(conn);

    assert_eq!(count, 2, "Two elements must have been inserted");

    let transactions = query_transactions(conn);

    assert_eq!(transactions.len(), 2, "Two elements must be in the DB");
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_select_nullifiers() {
    let mut conn = create_db();
    let conn = &mut conn;
    let block_num = 1.into();
    create_block(conn, block_num);

    // test querying empty table
    let nullifiers = queries::select_all_nullifiers(conn).unwrap();
    assert!(nullifiers.is_empty());

    // test multiple entries
    let mut state = vec![];
    for i in 0..10 {
        let nullifier = num_to_nullifier(i);
        state.push(NullifierInfo { nullifier, block_num });

        let res = queries::insert_nullifiers_for_block(conn, &[nullifier], block_num);
        assert_eq!(res.unwrap(), 1, "One element must have been inserted");

        let nullifiers = queries::select_all_nullifiers(conn).unwrap();
        assert_eq!(nullifiers, state);
    }
}

pub fn create_note(account_id: AccountId) -> Note {
    let coin_seed: [u64; 4] = rand::rng().random();
    let rng = Arc::new(Mutex::new(RpoRandomCoin::new(coin_seed.map(Felt::new).into())));
    let mut rng = rng.lock().unwrap();
    create_p2id_note(
        account_id,
        account_id,
        vec![Asset::Fungible(
            FungibleAsset::new(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET.try_into().unwrap(), 10).unwrap(),
        )],
        NoteType::Public,
        Felt::default(),
        &mut *rng,
    )
    .expect("Failed to create note")
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_select_notes() {
    let mut conn = create_db();
    let conn = &mut conn;
    let block_num = BlockNumber::from(1);
    create_block(conn, block_num);

    // test querying empty table
    let notes = queries::select_all_notes(conn).unwrap();
    assert!(notes.is_empty());

    let account_id = AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap();

    queries::upsert_accounts(conn, &[mock_block_account_update(account_id, 0)], block_num).unwrap();

    let new_note = create_note(account_id);

    // test multiple entries
    let mut state = vec![];
    for i in 0..10 {
        let note = NoteRecord {
            block_num,
            note_index: BlockNoteIndex::new(0, i.try_into().unwrap()).unwrap(),
            note_id: num_to_word(u64::try_from(i).unwrap()),
            metadata: *new_note.metadata(),
            details: Some(NoteDetails::from(&new_note)),
            inclusion_path: SparseMerklePath::default(),
        };
        state.push(note.clone());

        // insert scripts (after the first iteration the script is already in the db)
        let res = queries::insert_scripts(conn, [&note]);
        if i == 0 {
            assert_eq!(res.unwrap(), 1, "One element must have been inserted");
        } else {
            assert_eq!(res.unwrap(), 0, "No new elements must have been inserted");
        }

        // insert notes
        let res = queries::insert_notes(conn, &[(note, None)]);
        assert_eq!(res.unwrap(), 1, "One element must have been inserted");

        let notes = queries::select_all_notes(conn).unwrap();
        assert_eq!(notes, state);
    }
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_select_notes_different_execution_hints() {
    let mut conn = create_db();
    let conn = &mut conn;

    let block_num = 1.into();
    create_block(conn, block_num);

    // test querying empty table
    let notes = queries::select_all_notes(conn).unwrap();
    assert!(notes.is_empty());

    let sender = AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap();

    queries::upsert_accounts(conn, &[mock_block_account_update(sender, 0)], block_num).unwrap();

    // test multiple entries
    let mut state = vec![];

    let new_note = create_note(sender);

    let note_none = NoteRecord {
        block_num,
        note_index: BlockNoteIndex::new(0, 0).unwrap(),
        note_id: num_to_word(0),
        metadata: NoteMetadata::new(
            sender,
            NoteType::Public,
            0.into(),
            NoteExecutionHint::none(),
            Felt::default(),
        )
        .unwrap(),
        details: Some(NoteDetails::from(&new_note)),
        inclusion_path: SparseMerklePath::default(),
    };
    state.push(note_none.clone());

    queries::insert_scripts(conn, [&note_none]).unwrap(); // only necessary for the first note
    let res = queries::insert_notes(conn, &[(note_none, None)]);
    assert_eq!(res.unwrap(), 1, "One element must have been inserted");
<<<<<<< HEAD
    transaction.commit().unwrap();
    let note = &sql::select_notes_by_id(&conn.transaction().unwrap(), &[num_to_word(0).into()])
        .unwrap()[0];
=======

    let note = &queries::select_notes_by_id(conn, &[num_to_rpo_digest(0).into()]).unwrap()[0];
>>>>>>> b1ad1a38 (migrate to diesel-rs)
    assert_eq!(note.metadata.execution_hint(), NoteExecutionHint::none());

    let note_always = NoteRecord {
        block_num,
        note_index: BlockNoteIndex::new(0, 1).unwrap(),
        note_id: num_to_word(1),
        metadata: NoteMetadata::new(
            sender,
            NoteType::Public,
            0.into(),
            NoteExecutionHint::always(),
            Felt::default(),
        )
        .unwrap(),
        details: Some(NoteDetails::from(&new_note)),
        inclusion_path: SparseMerklePath::default(),
    };
    state.push(note_always.clone());

    let res = queries::insert_notes(conn, &[(note_always, None)]);
    assert_eq!(res.unwrap(), 1, "One element must have been inserted");
<<<<<<< HEAD
    transaction.commit().unwrap();
    let note = &sql::select_notes_by_id(&conn.transaction().unwrap(), &[num_to_word(1).into()])
        .unwrap()[0];
=======

    let note = &queries::select_notes_by_id(conn, &[num_to_rpo_digest(1).into()]).unwrap()[0];
>>>>>>> b1ad1a38 (migrate to diesel-rs)
    assert_eq!(note.metadata.execution_hint(), NoteExecutionHint::always());

    let note_after_block = NoteRecord {
        block_num,
        note_index: BlockNoteIndex::new(0, 2).unwrap(),
        note_id: num_to_word(2),
        metadata: NoteMetadata::new(
            sender,
            NoteType::Public,
            2.into(),
            NoteExecutionHint::after_block(12.into()).unwrap(),
            Felt::default(),
        )
        .unwrap(),
        details: Some(NoteDetails::from(&new_note)),
        inclusion_path: SparseMerklePath::default(),
    };
    state.push(note_after_block.clone());

    let res = queries::insert_notes(conn, &[(note_after_block, None)]);
    assert_eq!(res.unwrap(), 1, "One element must have been inserted");
<<<<<<< HEAD
    transaction.commit().unwrap();
    let note = &sql::select_notes_by_id(&conn.transaction().unwrap(), &[num_to_word(2).into()])
        .unwrap()[0];
=======

    let note = &queries::select_notes_by_id(conn, &[num_to_rpo_digest(2).into()]).unwrap()[0];
>>>>>>> b1ad1a38 (migrate to diesel-rs)
    assert_eq!(
        note.metadata.execution_hint(),
        NoteExecutionHint::after_block(12.into()).unwrap()
    );
}

// Generates an account, inserts into the database, and creates a note for it.
fn make_account_and_note(
    conn: &mut SqliteConnection,
    block_num: BlockNumber,
    init_seed: [u8; 32],
    storage_mode: AccountStorageMode,
) -> (AccountId, Note) {
    conn.transaction(|conn| {
        let account = mock_account_code_and_storage(
            AccountType::RegularAccountUpdatableCode,
            storage_mode,
            [],
            Some(init_seed),
        );
        let account_id = account.id();
        queries::upsert_accounts(
            conn,
            &[BlockAccountUpdate::new(
                account_id,
                account.commitment(),
                AccountUpdateDetails::New(account),
            )],
            block_num,
        )
        .unwrap();

        let new_note = create_note(account_id);
        Ok::<_, DatabaseError>((account_id, new_note))
    })
    .unwrap()
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_unconsumed_network_notes() {
    // Number of notes to generate.
    const N: u64 = 32;

    let mut conn = create_db();
    let conn = &mut conn;

    let block_num = BlockNumber::from(1);
    // An arbitrary public account (network note tag requires public account).
    create_block(conn, block_num);

    let account_notes = vec![
        make_account_and_note(conn, block_num, [0u8; 32], AccountStorageMode::Public),
        make_account_and_note(conn, block_num, [1u8; 32], AccountStorageMode::Network),
    ];

    // Create some notes, of which half are network notes.
    let notes = (0..N)
        .map(|i| {
            let index = (i % 2) as usize;
            let is_network = account_notes[index].0.storage_mode() == AccountStorageMode::Network;
            let account_id = account_notes[index].0;
            let new_note = &account_notes[index].1;
            let note = NoteRecord {
                block_num,
                note_index: BlockNoteIndex::new(0, i as usize).unwrap(),
                note_id: num_to_word(i),
                metadata: NoteMetadata::new(
                    account_notes[index].0,
                    NoteType::Public,
                    NoteTag::from_account_id(account_id),
                    NoteExecutionHint::none(),
                    Felt::default(),
                )
                .unwrap(),
                details: is_network.then_some(NoteDetails::from(new_note)),
                inclusion_path: SparseMerklePath::default(),
            };

            (note, is_network.then_some(num_to_nullifier(i)))
        })
        .collect::<Vec<_>>();

    // Copy out all network notes to assert against. These will be in chronological order already.
    let network_notes = notes
        .iter()
        .filter_map(|(note, nullifier)| nullifier.is_some().then_some(note.clone()))
        .collect::<Vec<_>>();

    // Insert the set of notes.
    queries::insert_scripts(conn, notes.iter().map(|(note, _)| note)).unwrap();
    queries::insert_notes(conn, &notes).unwrap();

    // Fetch all network notes by setting a limit larger than the amount available.
    let (result, _) = queries::unconsumed_network_notes(
        conn,
        Page {
            token: None,
            size: NonZeroUsize::new(N as usize * 10).unwrap(),
        },
    )
    .unwrap();
    assert_eq!(result, network_notes);

    // Check pagination works as expected.
    let limit = 5;
    let mut page = Page {
        token: None,
        size: NonZeroUsize::new(limit).unwrap(),
    };
    network_notes.chunks(limit).for_each(|expected| {
        let (result, new_page) = queries::unconsumed_network_notes(conn, page).unwrap();
        page = new_page;
        assert_eq!(result, expected);
    });
    assert!(page.token.is_none());

    // Consume every third network note and ensure these are now excluded from the results.
    let consumed = notes
        .iter()
        .filter_map(|(_, nullifier)| *nullifier)
        .step_by(3)
        .collect::<Vec<_>>();
    queries::insert_nullifiers_for_block(conn, &consumed, block_num).unwrap();

    let expected = network_notes
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 3 != 0)
        .map(|(_, note)| note.clone())
        .collect::<Vec<_>>();
    let page = Page {
        token: None,
        size: NonZeroUsize::new(N as usize * 10).unwrap(),
    };
    let (result, _) = queries::unconsumed_network_notes(conn, page).unwrap();
    assert_eq!(result, expected);
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_select_accounts() {
    let mut conn = create_db();
    let conn = &mut conn;
    let block_num = 1.into();
    create_block(conn, block_num);

    // test querying empty table
    let accounts = queries::select_all_accounts(conn).unwrap();
    assert!(accounts.is_empty());
    // test multiple entries
    let mut state = vec![];
    for i in 0..10u8 {
        let account_id = AccountId::dummy(
            [i; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountImmutableCode,
            AccountStorageMode::Private,
        );
        let account_commitment = num_to_word(u64::from(i));
        state.push(AccountInfo {
            summary: AccountSummary {
                account_id,
                account_commitment,
                block_num,
            },
            details: None,
        });

        let res = queries::upsert_accounts(
            conn,
            &[BlockAccountUpdate::new(
                account_id,
                account_commitment,
                AccountUpdateDetails::Private,
            )],
            block_num,
        );
        assert_eq!(res.unwrap(), 1, "One element must have been inserted");

        let accounts = queries::select_all_accounts(conn).unwrap();
        assert_eq!(accounts, state);
    }
}

#[test]
#[miden_node_test_macro::enable_logging]
fn sql_public_account_details() {
    let mut conn = create_db();
    let conn = &mut conn;
    create_block(conn, 1.into());

    let fungible_faucet_id = AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).unwrap();
    let non_fungible_faucet_id =
        AccountId::try_from(ACCOUNT_ID_PUBLIC_NON_FUNGIBLE_FAUCET).unwrap();

    let nft1 = Asset::NonFungible(
        NonFungibleAsset::new(
            &NonFungibleAssetDetails::new(non_fungible_faucet_id.prefix(), vec![1, 2, 3]).unwrap(),
        )
        .unwrap(),
    );

    let mut account = mock_account_code_and_storage(
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Public,
        [Asset::Fungible(FungibleAsset::new(fungible_faucet_id, 150).unwrap()), nft1],
        None,
    );

    // test querying empty table
    let accounts_in_db = queries::select_all_accounts(conn).unwrap();
    assert!(accounts_in_db.is_empty());

    let inserted = queries::upsert_accounts(
        conn,
        &[BlockAccountUpdate::new(
            account.id(),
            account.commitment(),
            AccountUpdateDetails::New(account.clone()),
        )],
        1.into(),
    )
    .unwrap();

    assert_eq!(inserted, 1, "One element must have been inserted");

    let mut accounts_in_db = queries::select_all_accounts(conn).unwrap();

    assert_eq!(accounts_in_db.len(), 1, "One element must have been inserted");

    let account_read = accounts_in_db.pop().unwrap().details.unwrap();
    assert_eq!(account_read, account);
    assert_eq!(account_read.nonce(), Felt::try_from(1_u64).unwrap());

    create_block(conn, 2.into());

    let read_delta = queries::select_account_delta(conn, account.id(), 1.into(), 2.into()).unwrap();

    assert_eq!(read_delta, None);

    let storage_delta =
        AccountStorageDelta::from_iters([3], [(4, num_to_word(5)), (5, num_to_word(6))], []);

    let nft2 = Asset::NonFungible(
        NonFungibleAsset::new(
            &NonFungibleAssetDetails::new(non_fungible_faucet_id.prefix(), vec![4, 5, 6]).unwrap(),
        )
        .unwrap(),
    );

    let vault_delta = AccountVaultDelta::from_iters([nft2], [nft1]);

    let mut delta2 =
        AccountDelta::new(account.id(), storage_delta, vault_delta, Felt::new(2)).unwrap();

    account.apply_delta(&delta2).unwrap();

    assert_eq!(dbg!(account.nonce()), Felt::try_from(3_u64).unwrap());

    let inserted = queries::upsert_accounts(
        conn,
        &[BlockAccountUpdate::new(
            account.id(),
            account.commitment(),
            AccountUpdateDetails::Delta(delta2.clone()),
        )],
        2.into(),
    )
    .unwrap();

    assert_eq!(inserted, 1, "One element must have been inserted");

    let mut accounts_in_db = queries::select_all_accounts(conn).unwrap();

    assert_eq!(accounts_in_db.len(), 1, "One element must have been inserted");

    let account_read = accounts_in_db.pop().unwrap().details.unwrap();

    assert_eq!(account_read.id(), account.id());
    assert_eq!(account_read.vault(), account.vault());
    assert_eq!(account_read.nonce(), account.nonce());
    assert_eq!(account_read.storage(), account.storage());

    let read_delta = queries::select_account_delta(conn, account.id(), 1.into(), 2.into()).unwrap();
    assert_eq!(read_delta.as_ref(), Some(&delta2));

    create_block(conn, 3.into());

    let storage_delta3 = AccountStorageDelta::from_iters([5], [], []);

    let delta3 = AccountDelta::new(
        account.id(),
        storage_delta3,
        AccountVaultDelta::from_iters([nft1], []),
        Felt::new(3),
    )
    .unwrap();

    account.apply_delta(&delta3).unwrap();

    let inserted = queries::upsert_accounts(
        conn,
        &[BlockAccountUpdate::new(
            account.id(),
            account.commitment(),
            AccountUpdateDetails::Delta(delta3.clone()),
        )],
        3.into(),
    )
    .unwrap();

    assert_eq!(inserted, 1, "One element must have been inserted");

    let mut accounts_in_db = queries::select_all_accounts(conn).unwrap();

    assert_eq!(accounts_in_db.len(), 1, "One element must have been inserted");

    let account_read = accounts_in_db.pop().unwrap().details.unwrap();

    assert_eq!(account_read.id(), account.id());
    assert_eq!(account_read.vault(), account.vault());
    assert_eq!(account_read.nonce(), account.nonce());

    let read_delta = queries::select_account_delta(conn, account.id(), 1.into(), 3.into()).unwrap();

    delta2.merge(delta3).unwrap();

    assert_eq!(read_delta, Some(delta2));
}

#[test]
#[miden_node_test_macro::enable_logging]
fn select_nullifiers_by_prefix_works() {
    const PREFIX_LEN: u8 = 16;
    let mut conn = create_db();
    let conn = &mut conn; // test empty table
    let block_number0 = 0.into();
    let nullifiers =
        queries::select_nullifiers_by_prefix(conn, PREFIX_LEN, &[], block_number0).unwrap();
    assert!(nullifiers.is_empty());

    // test single item
    let nullifier1 = num_to_nullifier(1 << 48);
    let block_number1 = 1.into();
    create_block(conn, block_number1);

    queries::insert_nullifiers_for_block(conn, &[nullifier1], block_number1).unwrap();

    let nullifiers = queries::select_nullifiers_by_prefix(
        conn,
        PREFIX_LEN,
        &[utils::get_nullifier_prefix(&nullifier1)],
        block_number0,
    )
    .unwrap();
    assert_eq!(
        nullifiers,
        vec![NullifierInfo {
            nullifier: nullifier1,
            block_num: block_number1
        }]
    );

    // test two elements
    let nullifier2 = num_to_nullifier(2 << 48);
    let block_number2 = 2.into();
    create_block(conn, block_number2);

    queries::insert_nullifiers_for_block(conn, &[nullifier2], block_number2).unwrap();

    let nullifiers = queries::select_all_nullifiers(conn).unwrap();
    assert_eq!(nullifiers, vec![(nullifier1, block_number1), (nullifier2, block_number2)]);

    // only the nullifiers matching the prefix are included
    let nullifiers = queries::select_nullifiers_by_prefix(
        conn,
        PREFIX_LEN,
        &[utils::get_nullifier_prefix(&nullifier1)],
        block_number0,
    )
    .unwrap();
    assert_eq!(
        nullifiers,
        vec![NullifierInfo {
            nullifier: nullifier1,
            block_num: block_number1
        }]
    );
    let nullifiers = queries::select_nullifiers_by_prefix(
        conn,
        PREFIX_LEN,
        &[utils::get_nullifier_prefix(&nullifier2)],
        block_number0,
    )
    .unwrap();
    assert_eq!(
        nullifiers,
        vec![NullifierInfo {
            nullifier: nullifier2,
            block_num: block_number2
        }]
    );

    // All matching nullifiers are included
    let nullifiers = queries::select_nullifiers_by_prefix(
        conn,
        PREFIX_LEN,
        &[
            utils::get_nullifier_prefix(&nullifier1),
            utils::get_nullifier_prefix(&nullifier2),
        ],
        block_number0,
    )
    .unwrap();
    assert_eq!(
        nullifiers,
        vec![
            NullifierInfo {
                nullifier: nullifier1,
                block_num: block_number1
            },
            NullifierInfo {
                nullifier: nullifier2,
                block_num: block_number2
            }
        ]
    );

    // If a non-matching prefix is provided, no nullifiers are returned
    let nullifiers = queries::select_nullifiers_by_prefix(
        conn,
        PREFIX_LEN,
        &[utils::get_nullifier_prefix(&num_to_nullifier(3 << 48))],
        block_number0,
    )
    .unwrap();
    assert!(nullifiers.is_empty());

    // If a block number is provided, only matching nullifiers created at or after that block are
    // returned
    let nullifiers = queries::select_nullifiers_by_prefix(
        conn,
        PREFIX_LEN,
        &[
            utils::get_nullifier_prefix(&nullifier1),
            utils::get_nullifier_prefix(&nullifier2),
        ],
        block_number2,
    )
    .unwrap();
    assert_eq!(
        nullifiers,
        vec![NullifierInfo {
            nullifier: nullifier2,
            block_num: block_number2
        }]
    );
}

#[test]
#[miden_node_test_macro::enable_logging]
fn db_block_header() {
    let mut conn = create_db();
    let conn = &mut conn; // test querying empty table
    let block_number = 1;
    let res = queries::select_block_header_by_block_num(conn, Some(block_number.into())).unwrap();
    assert!(res.is_none());

    let res = queries::select_block_header_by_block_num(conn, None).unwrap();
    assert!(res.is_none());

    let res = queries::select_all_block_headers(conn).unwrap();
    assert!(res.is_empty());

    let block_header = BlockHeader::new(
        1_u8.into(),
        num_to_word(2),
        3.into(),
        num_to_word(4),
        num_to_word(5),
        num_to_word(6),
        num_to_word(7),
        num_to_word(8),
        num_to_word(9),
        num_to_word(10),
        11_u8.into(),
    );
    // test insertion

    queries::insert_block_header(conn, &block_header).unwrap();

    // test fetch unknown block header
    let block_number = 1;
    let res = queries::select_block_header_by_block_num(conn, Some(block_number.into())).unwrap();
    assert!(res.is_none());

    // test fetch block header by block number
    let res =
        queries::select_block_header_by_block_num(conn, Some(block_header.block_num())).unwrap();
    assert_eq!(res.unwrap(), block_header);

    // test fetch latest block header
    let res = queries::select_block_header_by_block_num(conn, None).unwrap();
    assert_eq!(res.unwrap(), block_header);

    let block_header2 = BlockHeader::new(
        11_u8.into(),
        num_to_word(12),
        13.into(),
        num_to_word(14),
        num_to_word(15),
        num_to_word(16),
        num_to_word(17),
        num_to_word(18),
        num_to_word(19),
        num_to_word(20),
        21_u8.into(),
    );

    queries::insert_block_header(conn, &block_header2).unwrap();

    let res = queries::select_block_header_by_block_num(conn, None).unwrap();
    assert_eq!(res.unwrap(), block_header2);

    let res = queries::select_all_block_headers(conn).unwrap();
    assert_eq!(res, [block_header, block_header2]);
}

#[test]
#[miden_node_test_macro::enable_logging]
fn db_account() {
    let mut conn = create_db();
    let conn = &mut conn;
    let block_num: BlockNumber = 1.into();
    create_block(conn, block_num);

    // test empty table
    let account_ids: Vec<AccountId> =
        [ACCOUNT_ID_REGULAR_PRIVATE_ACCOUNT_UPDATABLE_CODE, 1, 2, 3, 4, 5]
            .iter()
            .map(|acc_id| (*acc_id).try_into().unwrap())
            .collect();
    let res =
        queries::select_accounts_by_block_range(conn, &account_ids, 0.into(), u32::MAX.into())
            .unwrap();
    assert!(res.is_empty());

    // test insertion
    let account_id = ACCOUNT_ID_REGULAR_PRIVATE_ACCOUNT_UPDATABLE_CODE;
    let account_commitment = num_to_word(0);

    let row_count = queries::upsert_accounts(
        conn,
        &[BlockAccountUpdate::new(
            account_id.try_into().unwrap(),
            account_commitment,
            AccountUpdateDetails::Private,
        )],
        block_num,
    )
    .unwrap();

    assert_eq!(row_count, 1);

    // test successful query
    let res =
        queries::select_accounts_by_block_range(conn, &account_ids, 0.into(), u32::MAX.into())
            .unwrap();
    assert_eq!(
        res,
        vec![AccountSummary {
            account_id: account_id.try_into().unwrap(),
            account_commitment,
            block_num,
        }]
    );

    // test query for update outside the block range
    let res = queries::select_accounts_by_block_range(
        conn,
        &account_ids,
        (block_num.as_u32() + 1).into(),
        u32::MAX.into(),
    )
    .unwrap();
    assert!(res.is_empty());

    // test query with unknown accounts
    let res = queries::select_accounts_by_block_range(
        conn,
        &[6.try_into().unwrap(), 7.try_into().unwrap(), 8.try_into().unwrap()],
        (block_num.as_u32() + 1).into(),
        u32::MAX.into(),
    )
    .unwrap();
    assert!(res.is_empty());
}

#[test]
#[miden_node_test_macro::enable_logging]
fn notes() {
    let mut conn = create_db();
    let conn = &mut conn;
    let block_num_1 = 1.into();
    create_block(conn, block_num_1);

    // test empty table
    let res =
        queries::select_notes_since_block_by_tag_and_sender(conn, BlockNumber::from(0), &[], &[])
            .unwrap();

    assert!(res.is_empty());

    let res = queries::select_notes_since_block_by_tag_and_sender(
        conn,
        BlockNumber::from(0),
        &[],
        &[1, 2, 3],
    )
    .unwrap();
    assert!(res.is_empty());

    let sender = AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap();

    // test insertion

    queries::upsert_accounts(conn, &[mock_block_account_update(sender, 0)], block_num_1).unwrap();

    let new_note = create_note(sender);
    let note_index = BlockNoteIndex::new(0, 2).unwrap();
    let tag = 5u32;
    let note_metadata =
        NoteMetadata::new(sender, NoteType::Public, tag.into(), NoteExecutionHint::none(), ZERO)
            .unwrap();

    let values = [(note_index, new_note.id(), note_metadata)];
    let notes_db = BlockNoteTree::with_entries(values.iter().copied()).unwrap();
    let inclusion_path = notes_db.open(note_index);

    let note = NoteRecord {
        block_num: block_num_1,
        note_index,
        note_id: new_note.id().into(),
        metadata: NoteMetadata::new(
            sender,
            NoteType::Public,
            tag.into(),
            NoteExecutionHint::none(),
            Felt::default(),
        )
        .unwrap(),
        details: Some(NoteDetails::from(&new_note)),
        inclusion_path: inclusion_path.clone(),
    };

    queries::insert_scripts(conn, [&note]).unwrap();
    queries::insert_notes(conn, &[(note.clone(), None)]).unwrap();

    // test empty tags
    let res =
        queries::select_notes_since_block_by_tag_and_sender(conn, BlockNumber::from(0), &[], &[])
            .unwrap();
    assert!(res.is_empty());

    // test no updates
    let res = queries::select_notes_since_block_by_tag_and_sender(conn, block_num_1, &[], &[tag])
        .unwrap();
    assert!(res.is_empty());

    // test match
    let res = queries::select_notes_since_block_by_tag_and_sender(
        conn,
        block_num_1.parent().unwrap(),
        &[],
        &[tag],
    )
    .unwrap();
    assert_eq!(res, vec![note.clone().into()]);

    let block_num_2 = note.block_num + 1;
    create_block(conn, block_num_2);

    // insertion second note with same tag, but on higher block
    let note2 = NoteRecord {
        block_num: block_num_2,
        note_index: note.note_index,
        note_id: new_note.id().into(),
        metadata: note.metadata,
        details: None,
        inclusion_path: inclusion_path.clone(),
    };

    queries::insert_notes(conn, &[(note2.clone(), None)]).unwrap();

    // only first note is returned
    let res = queries::select_notes_since_block_by_tag_and_sender(
        conn,
        block_num_1.parent().unwrap(),
        &[],
        &[tag],
    )
    .unwrap();
    assert_eq!(res, vec![note.clone().into()]);

    // only the second note is returned
    let res = queries::select_notes_since_block_by_tag_and_sender(conn, block_num_1, &[], &[tag])
        .unwrap();
    assert_eq!(res, vec![note2.clone().into()]);
    // test query notes by id
    let notes = vec![note.clone(), note2];

    let note_ids: Vec<NoteId> = note_ids.into_iter().map(|note| note.note_id).map(From::from).collect();

    let res = queries::select_notes_by_id(conn, &note_ids).unwrap();
    assert_eq!(res, notes);

    // test notes have correct details
    let note_0 = res[0].clone();
    let note_1 = res[1].clone();
    assert_eq!(note_0.details, note.details);
    assert_eq!(note_1.details, None);
}

// UTILITIES
// -------------------------------------------------------------------------------------------
fn num_to_word(n: u64) -> Word {
    [Felt::ZERO, Felt::ZERO, Felt::ZERO, Felt::new(n)].into()
}

fn num_to_nullifier(n: u64) -> Nullifier {
    Nullifier::from(num_to_word(n))
}

fn mock_block_account_update(account_id: AccountId, num: u64) -> BlockAccountUpdate {
    BlockAccountUpdate::new(account_id, num_to_word(num), AccountUpdateDetails::Private)
}

fn mock_block_transaction(account_id: AccountId, num: u64) -> TransactionHeader {
    let initial_state_commitment = Word::try_from([num, 0, 0, 0]).unwrap();
    let final_account_commitment = Word::try_from([0, num, 0, 0]).unwrap();
    let input_notes_commitment = Word::try_from([0, 0, num, 0]).unwrap();
    let output_notes_commitment = Word::try_from([0, 0, 0, num]).unwrap();

    TransactionHeader::new_unchecked(
        TransactionId::new(
            initial_state_commitment,
            final_account_commitment,
            input_notes_commitment,
            output_notes_commitment,
        ),
        account_id,
        initial_state_commitment,
        final_account_commitment,
        vec![num_to_nullifier(num)],
        vec![NoteId::new(
            Word::try_from([num, num, 0, 0]).unwrap(),
            Word::try_from([0, 0, num, num]).unwrap(),
        )],
    )
}

fn insert_transactions(conn: &mut SqliteConnection) -> usize {
    let block_num = 1.into();
    create_block(conn, block_num);

    conn.transaction(|conn| {
        let account_id = AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap();

        let account_updates = vec![mock_block_account_update(account_id, 1)];

        let mock_tx1 =
            mock_block_transaction(AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap(), 1);
        let mock_tx2 =
            mock_block_transaction(AccountId::try_from(ACCOUNT_ID_PRIVATE_SENDER).unwrap(), 2);
        let ordered_tx_headers = OrderedTransactionHeaders::new_unchecked(vec![mock_tx1, mock_tx2]);

        queries::upsert_accounts(conn, &account_updates, block_num).unwrap();

        let count = queries::insert_transactions(conn, block_num, &ordered_tx_headers).unwrap();
        Ok::<_, DatabaseError>(count)
    })
    .unwrap()
}

fn mock_account_code_and_storage(
    account_type: AccountType,
    storage_mode: AccountStorageMode,
    assets: impl IntoIterator<Item = Asset>,
    init_seed: Option<[u8; 32]>,
) -> Account {
    let component_code = "\
    export.account_procedure_1
        push.1.2
        add
    end
    ";

    let component_storage = vec![
        StorageSlot::Value(Word::empty()),
        StorageSlot::Value(num_to_word(1)),
        StorageSlot::Value(Word::empty()),
        StorageSlot::Value(num_to_word(3)),
        StorageSlot::Value(Word::empty()),
        StorageSlot::Value(num_to_word(5)),
    ];

    let component = AccountComponent::compile(
        component_code,
        TransactionKernel::testing_assembler(),
        component_storage,
    )
    .unwrap()
    .with_supported_type(account_type);

    AccountBuilder::new(init_seed.unwrap_or([0; 32]))
        .account_type(account_type)
        .storage_mode(storage_mode)
        .with_assets(assets)
        .with_component(component)
        .with_auth_component(AuthRpoFalcon512::new(PublicKey::new(EMPTY_WORD)))
        .build_existing()
        .unwrap()
}
