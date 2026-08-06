from django.apps import apps
from django.test import SimpleTestCase

from accounts.models import User

from .models import Author, Blog, Route, Tag, Team


class ModelMetadataTests(SimpleTestCase):
    def test_models_and_relations_are_valid(self):
        self.assertIs(apps.get_model("blog", "Blog"), Blog)
        self.assertIs(Blog._meta.get_field("author").related_model, Author)
        self.assertIs(Author._meta.get_field("team").related_model, Team)
        self.assertIs(Blog._meta.get_field("tags").related_model, Tag)
        self.assertIs(Route._meta.get_field("installer").related_model, User)

    def test_reverse_query_configuration_is_valid(self):
        relation = Blog._meta.get_field("author").remote_field
        self.assertEqual(relation.related_name, "blogs")
        self.assertEqual(relation.related_query_name, "authored_blogs")
