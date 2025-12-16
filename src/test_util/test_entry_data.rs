use std::{collections::HashMap, io::Write, ops::Range, path::Path};

use indexmap::IndexMap;

use crate::{Builder, Reader};
use proptest::{
    arbitrary::Arbitrary,
    strategy::{BoxedStrategy, Just, MapInto, Strategy},
};
use tempfile::TempDir;

#[derive(Clone, Debug)]
pub struct TestEntryData(pub IndexMap<String, Vec<u8>>);

/// A type that holds both a reader and a hash map with the entries the reader contains.
#[derive(Clone, Debug)]
pub struct ReaderAndData {
    pub reader: Reader<Vec<u8>>,
    pub data: TestEntryData,
}

#[derive(Clone, Debug)]
pub struct ArbitraryTestEntryDataParams {
    /// How many entries to generate
    pub count_range: Range<usize>,

    /// How much data can an entry have
    pub max_size: usize,

    /// Use only simple printable nonempty ASCII strings for filenames
    pub entry_name_pattern: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct ArbitraryReaderParams {
    /// Parameters for generating the entries
    pub entries: ArbitraryTestEntryDataParams,
    /// Controls wheter the arbitrary implementation should also seek the reader to random position.
    pub seek: bool,
}

impl Arbitrary for TestEntryData {
    type Parameters = ArbitraryTestEntryDataParams;
    type Strategy = BoxedStrategy<TestEntryData>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        let content_strategy =
            proptest::collection::vec(proptest::bits::u8::ANY, 0..=args.max_size).boxed();

        proptest::collection::hash_map(args.entry_name_pattern, content_strategy, args.count_range)
            .prop_map_into()
            .boxed()
    }
}

impl Arbitrary for ReaderAndData {
    type Parameters = ArbitraryReaderParams;
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        let fresh_reader_strategy =
            TestEntryData::arbitrary_with(args.clone().into()).prop_map_into();

        if args.seek {
            fresh_reader_strategy
                .prop_flat_map(|reader_and_data: ReaderAndData| {
                    let len = reader_and_data.reader.size();
                    (Just(reader_and_data), 0..len)
                })
                .prop_map(|(mut reader_and_data, seek_offset)| {
                    reader_and_data.reader.seek_from_start_mut(seek_offset);
                    reader_and_data
                })
                .boxed()
        } else {
            fresh_reader_strategy.boxed()
        }
    }
}

impl Arbitrary for Reader<Vec<u8>> {
    type Parameters = ArbitraryReaderParams;
    type Strategy = MapInto<BoxedStrategy<ReaderAndData>, Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        ReaderAndData::arbitrary_with(args).prop_map_into()
    }
}

impl Default for ArbitraryTestEntryDataParams {
    fn default() -> Self {
        ArbitraryTestEntryDataParams {
            count_range: 0..64,
            max_size: 512,
            entry_name_pattern: ".{0,30}",
        }
    }
}

impl From<ReaderAndData> for Reader<Vec<u8>> {
    fn from(value: ReaderAndData) -> Self {
        value.reader
    }
}

impl From<HashMap<String, Vec<u8>>> for TestEntryData {
    fn from(value: HashMap<String, Vec<u8>>) -> Self {
        TestEntryData(value.into_iter().collect())
    }
}

impl From<TestEntryData> for Builder<Vec<u8>> {
    fn from(value: TestEntryData) -> Self {
        let mut builder = Builder::new();

        value.0.into_iter().for_each(|(name, content)| {
            builder
                .add_entry(name.clone(), content.clone())
                .expect("Adding entries from hash map should never fail");
        });

        builder
    }
}

impl From<TestEntryData> for Reader<Vec<u8>> {
    fn from(value: TestEntryData) -> Self {
        Builder::from(value).build()
    }
}

impl From<ArbitraryReaderParams> for ArbitraryTestEntryDataParams {
    fn from(value: ArbitraryReaderParams) -> Self {
        value.entries
    }
}

impl From<TestEntryData> for ReaderAndData {
    fn from(value: TestEntryData) -> Self {
        let builder = Builder::from(value.clone());

        ReaderAndData {
            reader: builder.build(),
            data: value,
        }
    }
}

impl TestEntryData {
    /// Creates a temporary directory that contains files corresponding to the test entry data.
    pub fn make_directory(&self) -> std::io::Result<TempDir> {
        let tmp = TempDir::new()?;

        for (name, value) in &self.0 {
            let path = tmp.as_ref().join(Path::new(&name));
            std::fs::create_dir_all(path.parent().unwrap())?;
            let mut f = std::fs::File::create(path)?;
            f.write_all(value)?;
        }

        Ok(tmp)
    }
}
