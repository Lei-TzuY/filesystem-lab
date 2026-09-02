use filesystem_lab::{
    directory::{DirectoryError, DirectoryInvariantError, DirectoryTable},
    inode::{InodeKind, InodeTable},
};

#[test]
fn insert_lookup_and_remove_entry() {
    let mut inodes = InodeTable::new();
    let root = inodes.create(InodeKind::Directory).unwrap();
    let file = inodes.create(InodeKind::File).unwrap();
    let mut directories = DirectoryTable::new();

    directories.insert(root, "hello", file, &inodes).unwrap();
    assert_eq!(directories.lookup(root, "hello"), Some(file));
    assert_eq!(directories.entry_count(root), 1);
    directories.validate(&inodes).unwrap();

    assert_eq!(directories.remove(root, "hello", &inodes).unwrap(), file);
    assert_eq!(directories.lookup(root, "hello"), None);
    assert_eq!(directories.entry_count(root), 0);
    directories.validate(&inodes).unwrap();
}

#[test]
fn rejects_invalid_parent_target_and_names() {
    let mut inodes = InodeTable::new();
    let directory = inodes.create(InodeKind::Directory).unwrap();
    let file = inodes.create(InodeKind::File).unwrap();
    let missing = inodes.create(InodeKind::File).unwrap();
    inodes.remove(missing).unwrap();
    let mut directories = DirectoryTable::new();

    assert_eq!(
        directories.insert(file, "child", directory, &inodes),
        Err(DirectoryError::ParentNotDirectory(file))
    );
    assert_eq!(
        directories.insert(directory, "child", missing, &inodes),
        Err(DirectoryError::TargetNotFound(missing))
    );

    for name in ["", ".", "..", "a/b", "nul\0name"] {
        assert_eq!(
            directories.insert(directory, name, file, &inodes),
            Err(DirectoryError::InvalidName(name.to_owned()))
        );
    }
}

#[test]
fn duplicate_names_are_rejected_deterministically() {
    let mut inodes = InodeTable::new();
    let directory = inodes.create(InodeKind::Directory).unwrap();
    let first = inodes.create(InodeKind::File).unwrap();
    let second = inodes.create(InodeKind::File).unwrap();
    let mut directories = DirectoryTable::new();

    directories
        .insert(directory, "same", first, &inodes)
        .unwrap();
    assert_eq!(
        directories.insert(directory, "same", second, &inodes),
        Err(DirectoryError::EntryAlreadyExists {
            parent: directory,
            name: "same".to_owned(),
        })
    );
    assert_eq!(directories.lookup(directory, "same"), Some(first));
}

#[test]
fn validation_detects_dangling_inode_reference() {
    let mut inodes = InodeTable::new();
    let directory = inodes.create(InodeKind::Directory).unwrap();
    let file = inodes.create(InodeKind::File).unwrap();
    let mut directories = DirectoryTable::new();

    directories
        .insert(directory, "gone", file, &inodes)
        .unwrap();
    inodes.remove(file).unwrap();

    assert_eq!(
        directories.validate(&inodes),
        Err(DirectoryInvariantError::DanglingTarget {
            parent: directory,
            name: "gone".to_owned(),
            target: file,
        })
    );
}

#[test]
fn removing_missing_entry_is_reported() {
    let mut inodes = InodeTable::new();
    let directory = inodes.create(InodeKind::Directory).unwrap();
    let mut directories = DirectoryTable::new();

    assert_eq!(
        directories.remove(directory, "missing", &inodes),
        Err(DirectoryError::EntryNotFound {
            parent: directory,
            name: "missing".to_owned(),
        })
    );
}
